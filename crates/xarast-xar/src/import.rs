//! The mapping stage: a record tree becomes a [`xarast_doc::Document`].
//!
//! Everything below this module is model-independent. This is where the
//! typed records of [`Decoded`] turn into nodes, attributes and resources,
//! and it is the only part of the crate that knows `xarast-doc` exists.
//!
//! # The importer never touches the arena
//!
//! Every node, attribute and resource goes through [`DocumentBuilder`].
//! `xarast-doc` makes that structural rather than a convention —
//! `Tree::attach` and its siblings are `pub(crate)` — and it is what
//! guarantees an importer cannot produce an inconsistent document:
//! `DocumentBuilder::finish` either fails or returns a document whose
//! `validate()` has no errors.
//!
//! # Attributes are children, so scope is the tree
//!
//! `.xar` writes an object's attributes inside the object's own `DOWN`/`UP`
//! (`research/01 §6.1`). The model has exactly the same rule — an attribute
//! applies to its following siblings *and to its parent's own ink* — so
//! pushing a builder scope on descent and popping it on ascent is the whole
//! of the inheritance mapping. There is no attribute stack to maintain.
//!
//! # What look-ahead is for
//!
//! Three records describe the node that *encloses* them: `TAG_PATH_FLAGS`
//! (the path's first child), `TAG_SPREADINFORMATION` and `TAG_LAYERDETAILS`.
//! A `DocumentBuilder` is append-only by design, so rather than mutate a node
//! after the fact the mapper reads those children before emitting the parent.
//! The record tree is already fully materialised, which makes that free; it is
//! also what the original does, under the name `ReadPostChildren`.
//!
//! # The spread coordinate origin
//!
//! See [`spread_origin`]. It is **not** `(0, 0)` for the great majority of
//! real files, and the corpus says so out loud.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

use xarast_color::{Colour, ColourValue, Transparency};
use xarast_doc::{
    ArrowSpec, AttrSlot, AttrStack, AttrValue, BitmapData, BitmapId, BitmapInfo, BitmapResource,
    BuildLimits, DocumentBuilder, DocumentMeta, GridKind, GridNode, ImageFormat, LayerNode,
    NodeKind, OpaqueNode, OriginalEncoded, PageNode, Paint, PathNode, ProceduralParams, QuickShape,
    Ramp, RampStop, SpreadNode, TextItem, TextLayout, TextStoryNode, Tiling, TranspPaint,
    TypefaceRef,
};
use xarast_geom::{BiasGain, Cap, DashPattern, FillRule, Join, Matrix, Mp, Point, Rect, Vector};

use crate::colour::ColourRegistry;
use crate::decode::{
    Decoded, FillEffect, FillKind, FractalParams, GradientFill, GradientTransparency, LayerFlags,
    RegularShape, SpreadInformation, TextAttr, TextPlacement, decode,
};
use crate::diag::{DiagCode, DiagSink, Diagnostic, Severity};
use crate::error::XarError;
use crate::header::FileHeader;
use crate::paths::apply_path_flags;
use crate::reader::{BlockReport, ReaderLimits};
use crate::tags::{Ref, TagClass, class_of};
use crate::tree::{FileAnalysis, RecordNode, analyse};

/// How many [`NodeKind`] discriminants there are.
///
/// The counts in [`ImportReport::node_counts`] are indexed by
/// `NodeKind::discriminant()`, so this has to track the model. A test asserts
/// it does.
pub const NODE_KIND_COUNT: usize = 19;

/// The name of each [`NodeKind`] discriminant, in discriminant order.
///
/// These are facts about the model, never about a file, so a report built
/// from them is safe to commit.
pub const NODE_KIND_NAMES: [&str; NODE_KIND_COUNT] = [
    "Document",
    "Chapter",
    "Spread",
    "Page",
    "Layer",
    "Grid",
    "Path",
    "Shape",
    "QuickShape",
    "Bitmap",
    "Guideline",
    "Group",
    "Live",
    "ClipView",
    "TextStory",
    "TextLine",
    "TextItem",
    "Attr",
    "Opaque",
];

/// What an import is allowed to cost, and what it may leave out.
#[derive(Clone, Debug, Default)]
pub struct ImportOptions {
    /// Caps on the physical layer.
    pub reader: ReaderLimits,
    /// Caps on the document being built.
    pub build: BuildLimits,
    /// Stop at the first `Error` diagnostic instead of carrying on.
    pub strict: bool,
    /// Drop text records entirely. Useful for bisecting a file that will not
    /// open.
    pub skip_text: bool,
    /// Keep bitmap metadata but not the embedded bytes.
    pub skip_bitmaps: bool,
    /// Caps on the one decode the import does itself: a `TAG_DEFINEBITMAP_PNG`
    /// with an alpha channel is rewritten as a standard PNG
    /// ([`xarast_image::xar::normalise_xar_png`]). A PNG that trips them is
    /// kept verbatim, with a warning.
    pub bitmap_limits: xarast_image::DecodeLimits,
}

/// What one import did. Facts only: no coordinate, colour or string taken
/// out of the file ever reaches this type, for the reason
/// [`crate::diag`] explains.
#[derive(Clone, Debug)]
pub struct ImportReport {
    /// The file header.
    pub header: FileHeader,
    /// Records the physical layer handed out.
    pub records_read: u32,
    /// Records that produced a node, an attribute, a resource or a document
    /// setting — or that were consumed by the node they describe, or that
    /// were `TAG_DOWN`/`TAG_UP` and became a builder scope.
    ///
    /// `records_mapped + records_skipped + records_stripped` equals
    /// [`ImportReport::records_read`]; [`ImportReport::records_opaque`] is a
    /// subset of `records_mapped`.
    pub records_mapped: u32,
    /// Records deliberately dropped: view state, printing, editor hints.
    pub records_skipped: u32,
    /// Records dropped together with an atomic subtree, by the tree builder.
    pub records_stripped: u32,
    /// Records with no decoder that were kept verbatim as
    /// [`NodeKind::Opaque`] so that they round-trip.
    pub records_opaque: u32,
    /// Occurrences per tag.
    pub distinct_tags: BTreeMap<u32, u32>,
    /// One entry per compressed block.
    pub blocks: Vec<BlockReport>,
    /// The deepest record nesting.
    pub max_depth: usize,
    /// Records that became a node of the *record* tree, which is what
    /// `records_mapped` and `records_skipped` between them account for.
    pub tree_nodes: usize,
    /// Nodes in the finished document, including the root.
    pub nodes_built: usize,
    /// Nodes per [`NodeKind`] discriminant; see [`NODE_KIND_NAMES`].
    pub node_counts: [u32; NODE_KIND_COUNT],
    /// Palette entries registered.
    pub colours: usize,
    /// Bitmap resources registered.
    pub bitmaps: usize,
    /// The coordinate origin the last `TAG_SPREADINFORMATION` implied.
    pub spread_origin: Point,
    /// Attributes read from `TAG_CURRENTATTRIBUTES` blocks. They are the
    /// editor's current attributes, not document defaults, and are skipped.
    pub current_attributes: u32,
    /// How many of those differed from
    /// [`DefaultAttrs::xara_compatible`](xarast_doc::DefaultAttrs::xara_compatible).
    pub current_differing: u32,
    /// Everything the `.xar` layers found.
    pub diagnostics: Vec<Diagnostic>,
    /// Everything the builder found, in the model's shared vocabulary.
    pub build_diagnostics: Vec<xarast_doc::Diagnostic>,
    /// How many `validate()` errors the finished document has. Always zero:
    /// `DocumentBuilder::finish` fails rather than return a document with
    /// any.
    pub validation_errors: usize,
    /// How many `validate()` warnings it has. `AttrAfterInk` is expected and
    /// common.
    pub validation_warnings: usize,
    /// Wall-clock time for the whole import.
    pub duration: Duration,
}

impl ImportReport {
    /// How many error-severity `.xar` diagnostics were raised.
    #[must_use]
    pub fn errors(&self) -> usize {
        self.diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .count()
    }

    /// How many warning-severity `.xar` diagnostics were raised.
    #[must_use]
    pub fn warnings(&self) -> usize {
        self.diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Warning)
            .count()
    }

    /// Tags with no decoder whose [`TagClass`] is `Structural`.
    ///
    /// The number the phase's acceptance criteria count: an unhandled
    /// attribute loses a colour, an unhandled structural tag means the tree
    /// is not the tree the file describes.
    #[must_use]
    pub fn unknown_structural_tags(&self) -> Vec<u32> {
        let mut out: Vec<u32> = self
            .distinct_tags
            .keys()
            .copied()
            .filter(|&t| {
                !crate::decode::has_decoder(t) && class_of(t) == Some(TagClass::Structural)
            })
            .collect();
        out.sort_unstable();
        out
    }

    /// The count for one [`NodeKind`] discriminant.
    #[must_use]
    pub fn nodes_of(&self, discriminant: u8) -> u32 {
        self.node_counts
            .get(usize::from(discriminant))
            .copied()
            .unwrap_or(0)
    }
}

/// Reads a `.xar` file and builds a [`Document`](xarast_doc::Document).
///
/// # Errors
///
/// [`XarError`] when the byte stream cannot be walked, and
/// [`XarError::Build`] when the document could not be built within
/// [`ImportOptions::build`].
pub fn import(
    bytes: &[u8],
    opts: &ImportOptions,
) -> Result<(xarast_doc::Document, ImportReport), XarError> {
    let start = Instant::now();
    let analysis = analyse(bytes, opts.reader)?;
    map(&analysis, opts, start)
}

/// Builds a document from an analysis that has already been done.
///
/// `xar-dump` uses this so that one read of a file serves both the facts
/// report and the model dump.
///
/// # Errors
///
/// As [`import`].
pub fn map(
    analysis: &FileAnalysis,
    opts: &ImportOptions,
    started: Instant,
) -> Result<(xarast_doc::Document, ImportReport), XarError> {
    let mut m = Mapper::new(opts);
    m.diags.absorb(&analysis.diagnostics);
    m.builder.meta(DocumentMeta {
        producer: analysis.header.producer.clone(),
        producer_version: analysis.header.producer_version.clone(),
        producer_build: analysis.header.producer_build.clone(),
        ..DocumentMeta::default()
    });
    m.visit_children(&analysis.tree.roots)?;

    // `TAG_DOWN` and `TAG_UP` never reach the tree as nodes — the tree
    // builder turned them into nesting — but they are mapped all the same,
    // onto `push_scope`/`pop_scope`. What is left once the tree's own nodes
    // and the stripped subtrees are taken out is exactly those, so adding
    // them here makes `mapped + skipped + stripped == records_read` hold
    // *and* keeps it a real check: it can only balance if every tree node
    // was visited exactly once.
    let structural = analysis
        .records_read
        .saturating_sub(analysis.tree.stripped)
        .saturating_sub(u32::try_from(analysis.tree.nodes).unwrap_or(u32::MAX));
    m.mapped = m.mapped.saturating_add(structural);

    let colours = m.colours.len();
    let bitmaps = m.bitmap_by_record.len();
    let origin = m.origin;
    let mapped = m.mapped;
    let skipped = m.skipped;
    let opaque = m.opaque;
    let current_attributes = m.current_attributes;
    let current_differing = m.current_differing;
    let diagnostics = m.diags.items().to_vec();

    // The builder validated the document as its last step; reuse that
    // report rather than walk half a million nodes a second time.
    let (doc, build_diagnostics, validation) = m.builder.finish_with_report()?;

    let mut node_counts = [0u32; NODE_KIND_COUNT];
    let mut nodes_built = 0usize;
    let mut count = |k: &NodeKind| {
        nodes_built = nodes_built.saturating_add(1);
        if let Some(slot) = node_counts.get_mut(usize::from(k.discriminant())) {
            *slot = slot.saturating_add(1);
        }
    };
    if validation.reachable == doc.tree.node_count() {
        // Every node alive is reachable, so the arena, in memory order, is
        // the same set as a walk from the root and much cheaper to visit.
        for (_, data) in doc.tree.iter() {
            count(&data.kind);
        }
    } else {
        for id in doc.tree.preorder(doc.tree.root()) {
            if let Some(k) = doc.tree.kind(id) {
                count(k);
            }
        }
    }

    Ok((
        doc,
        ImportReport {
            header: analysis.header.clone(),
            records_read: analysis.records_read,
            records_mapped: mapped,
            records_skipped: skipped,
            records_stripped: analysis.tree.stripped,
            records_opaque: opaque,
            distinct_tags: analysis.tree.histogram.clone(),
            blocks: analysis.blocks.clone(),
            max_depth: analysis.tree.max_depth,
            tree_nodes: analysis.tree.nodes,
            nodes_built,
            node_counts,
            colours,
            bitmaps,
            spread_origin: origin,
            current_attributes,
            current_differing,
            diagnostics,
            build_diagnostics,
            validation_errors: validation.errors.len(),
            validation_warnings: validation.warnings.len(),
            duration: started.elapsed(),
        },
    ))
}

/// The coordinate origin every point in a spread is written relative to.
///
/// `research/01 §5.3` states the rule — *the `lo` corner of the rectangle
/// enclosing every page of the spread* — but not how to compute that corner
/// from the four numbers `TAG_SPREADINFORMATION` actually carries. It is:
///
/// ```text
/// page_margin = if margin < bleed { margin + bleed } else { margin }
/// origin      = (page_margin, page_margin)
/// ```
///
/// A spread lays its first page out with its `lo` corner at
/// `(page_margin, page_margin)` in spread coordinates, and a double page
/// spread puts the second page one page-width to the right of it, so the
/// union's `lo` corner is the first page's either way
/// (`Kernel/spread.cpp:2196-2290`, `Kernel/rechdoc.cpp:640-644`).
///
/// **The corpus distinguishes this from `(0, 0)` decisively.** Every empty
/// template writes `TAG_VIEWPORT` as the degenerate rectangle
/// `(-margin, -margin, -margin, -margin)` and `TAG_CURRENTATTRIBUTEBOUNDS` as
/// `(-margin, -margin)`: both are an empty `DocRect` at spread-coordinate
/// `(0, 0)` with the origin already subtracted by the writer. `margin` is
/// 566 931 or 576 000 in 57 of the 59 files and 0 in `animation.xar`, whose
/// viewport is correspondingly `(0, 0, 0, 0)`. The origin is the margin.
///
/// What the corpus cannot distinguish is `margin` from
/// `margin < bleed ? margin + bleed : margin`, because every corpus file has
/// `bleed == 0`. `crate::synth` builds a file that does, and the unit tests
/// use it.
#[must_use]
pub fn spread_origin(info: &SpreadInformation) -> Point {
    let m = page_margin(info);
    Point::new(m, m)
}

/// The pasteboard margin actually used to place the pages.
fn page_margin(info: &SpreadInformation) -> Mp {
    if info.margin.raw() < info.bleed.raw() {
        info.margin.saturating_add(info.bleed)
    } else {
        info.margin
    }
}

/// The rectangle enclosing every page of the spread, in document
/// coordinates.
#[must_use]
pub fn pages_rect(info: &SpreadInformation) -> Rect {
    let lo = spread_origin(info);
    let pages = if info.double_page_spread() { 2 } else { 1 };
    let width = info.width.mul_ratio(pages, 1);
    Rect::new(
        lo,
        Point::new(lo.x.saturating_add(width), lo.y.saturating_add(info.height)),
    )
}

/// Where a record's children go.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum After {
    /// Into the node just emitted.
    Child,
    /// Onto the same level: the record itself produced no node.
    Same,
    /// Into the document's default attribute block.
    Defaults,
    /// Nowhere: the record handled them itself, or they are not wanted.
    Drop,
}

struct Mapper<'o> {
    builder: DocumentBuilder,
    colours: ColourRegistry,
    bitmap_by_record: HashMap<u32, BitmapId>,
    font_by_record: HashMap<u32, Arc<TypefaceRef>>,
    origin: Point,
    diags: DiagSink,
    opts: &'o ImportOptions,
    mapped: u32,
    skipped: u32,
    opaque: u32,
    current_attributes: u32,
    current_differing: u32,
    /// The attribute state the emitted document puts in force at the
    /// builder's insertion point, mirrored as nodes are emitted. Text
    /// records need it to scope a string's attributes to that string.
    attrs: AttrStack,
    /// Values to restore before the next text record at a given scope
    /// depth: what a string's own attributes overrode (see
    /// [`Mapper::text_items`]).
    text_restore: Vec<(usize, AttrValue)>,
    /// A `TAG_TEXT_CHAR` high surrogate waiting for its low half.
    pending_high: Option<u16>,
    /// The profiles a `TAG_BLENDPROFILES` gave the next `TAG_BLEND`: the
    /// original writes them just before it (`Kernel/nodeblnd.cpp:3530-3545`).
    pending_blend_profiles: Option<[(f64, f64); 3]>,
}

impl<'o> Mapper<'o> {
    fn new(opts: &'o ImportOptions) -> Mapper<'o> {
        Mapper {
            builder: DocumentBuilder::new(opts.build.clone()),
            colours: ColourRegistry::new(),
            bitmap_by_record: HashMap::new(),
            font_by_record: HashMap::new(),
            origin: Point::ORIGIN,
            diags: DiagSink::new(),
            opts,
            mapped: 0,
            skipped: 0,
            opaque: 0,
            current_attributes: 0,
            current_differing: 0,
            attrs: AttrStack::with_defaults(&xar_defaults()),
            text_restore: Vec::new(),
            pending_high: None,
            pending_blend_profiles: None,
        }
    }

    // ── Scopes and attributes, mirrored ─────────────────────────────────────

    fn push_scope(&mut self) -> Result<(), XarError> {
        self.builder.push_scope()?;
        self.attrs.push_scope();
        Ok(())
    }

    fn pop_scope(&mut self) {
        let depth = self.attrs.depth();
        self.text_restore.retain(|(d, _)| *d < depth);
        if self.pending_high.take().is_some() {
            // A lone high surrogate ends the line: keep its place. The
            // error is only in the tree the builder refuses.
            let _ = self
                .builder
                .node(NodeKind::TextItem(TextItem::Char('\u{fffd}')));
        }
        self.builder.pop_scope();
        self.attrs.pop_scope();
    }

    fn apply_attribute(&mut self, v: AttrValue) -> Result<(), XarError> {
        self.attrs.push(Arc::new(v.clone()));
        self.builder.attribute(v)?;
        Ok(())
    }

    // ── Text items ──────────────────────────────────────────────────────────

    /// Emits the items one text record stands for, with the attributes the
    /// record carries as children scoped to **those items only**.
    ///
    /// In the format a string's or character's attributes are its children
    /// and apply to it alone; a string is a run of characters that carried
    /// identical attribute children (`Kernel/cxftext.cpp:1260-1300`, and
    /// `Kernel/impstr.cpp`, which copies the string's attributes onto every
    /// character on import). The model's attribute scope is "the following
    /// siblings", so the attributes go *before* the items, and whatever
    /// they overrode is restored before the next text record that does not
    /// set the same slot. Emitting them after the items, as the structural
    /// import did, handed every string's style to the string after it.
    ///
    /// Definitions among the children (a font, a colour) are registered;
    /// any other child is visited after the items, at the same level.
    fn text_items(&mut self, items: &[TextItem], node: &RecordNode) -> Result<(), XarError> {
        let depth = self.attrs.depth();
        let mut own: Vec<AttrValue> = Vec::new();
        let mut rest: Vec<&RecordNode> = Vec::new();
        for child in &node.children {
            let rec = &child.record;
            let at = (rec.number, rec.tag);
            let Ok(d) = decode(rec.tag, &rec.data, self.origin, &mut self.diags, at) else {
                rest.push(child);
                continue;
            };
            if self.definition(&d, rec) {
                self.mapped = self.mapped.saturating_add(1);
                self.skipped = self.skipped.saturating_add(count_subtree(&child.children));
                continue;
            }
            match self.attribute(&d, at) {
                Some(v) if v.slot().is_some() => {
                    self.mapped = self.mapped.saturating_add(1);
                    self.skipped = self.skipped.saturating_add(count_subtree(&child.children));
                    own.push(v);
                }
                _ => rest.push(child),
            }
        }
        // Restore what the previous record overrode, unless this one sets
        // the same slot anyway.
        let restore = std::mem::take(&mut self.text_restore);
        for (d, v) in restore {
            if d != depth {
                self.text_restore.push((d, v));
            } else if !own.iter().any(|o| o.slot() == v.slot()) {
                if *self.attrs.get(v.slot().unwrap_or(AttrSlot::FillGeometry)) != v {
                    self.apply_attribute(v)?;
                }
            } else {
                // Still pending: the record below overrides it again.
                self.text_restore.push((d, v));
            }
        }
        for v in own {
            let Some(slot) = v.slot() else { continue };
            let prev = self.attrs.get(slot).clone();
            if prev == v {
                continue;
            }
            if !self
                .text_restore
                .iter()
                .any(|(d, r)| *d == depth && r.slot() == Some(slot))
            {
                self.text_restore.push((depth, prev));
            }
            self.apply_attribute(v)?;
        }
        for item in items {
            self.builder.node(NodeKind::TextItem(*item))?;
        }
        for child in rest {
            self.visit(child)?;
        }
        Ok(())
    }

    /// A high surrogate's record: its definitions are registered and its
    /// attributes counted — the low half's record carries the same ones and
    /// they are applied there.
    fn visit_text_leftovers(&mut self, node: &RecordNode) -> Result<(), XarError> {
        for child in &node.children {
            let rec = &child.record;
            let at = (rec.number, rec.tag);
            let Ok(d) = decode(rec.tag, &rec.data, self.origin, &mut self.diags, at) else {
                self.visit(child)?;
                continue;
            };
            if self.definition(&d, rec) || self.attribute(&d, at).is_some() {
                self.mapped = self.mapped.saturating_add(1);
                self.skipped = self.skipped.saturating_add(count_subtree(&child.children));
            } else {
                self.visit(child)?;
            }
        }
        Ok(())
    }

    /// A high surrogate followed by anything but its low half is lone: it
    /// becomes U+FFFD, as an unpaired code unit always has.
    fn flush_high(&mut self) -> Result<(), XarError> {
        if self.pending_high.take().is_some() {
            self.builder
                .node(NodeKind::TextItem(TextItem::Char('\u{fffd}')))?;
        }
        Ok(())
    }

    // ── Traversal ───────────────────────────────────────────────────────────

    fn visit_children(&mut self, children: &[RecordNode]) -> Result<(), XarError> {
        // `TAG_GRIDRULERSETTINGS` (46) and `TAG_GRIDRULERORIGIN` (47) are two
        // sibling records describing one `Grid` node, so the first of them
        // emits the node and the other is consumed.
        let grid_at = children
            .iter()
            .position(|c| matches!(c.record.tag, 46 | 47));
        for (i, child) in children.iter().enumerate() {
            if matches!(child.record.tag, 46 | 47) {
                if grid_at == Some(i) {
                    self.emit_grid(children)?;
                }
                self.mapped = self.mapped.saturating_add(1);
                // A grid record has no children in any real file; anything
                // a corrupt one nests under it is not visited, so it is
                // accounted for here as skipped.
                self.skipped = self.skipped.saturating_add(count_subtree(&child.children));
                continue;
            }
            self.visit(child)?;
        }
        Ok(())
    }

    fn visit(&mut self, node: &RecordNode) -> Result<(), XarError> {
        let rec = &node.record;
        let at = (rec.number, rec.tag);
        let mut decoded = match decode(rec.tag, &rec.data, self.origin, &mut self.diags, at) {
            Ok(d) => d,
            Err(_) => {
                self.diags.push(
                    Diagnostic::new(DiagCode::TruncatedRecord)
                        .at(rec.number, rec.tag)
                        .with_detail(rec.data.len() as u64),
                );
                self.skipped = self.skipped.saturating_add(1);
                return self.visit_children(&node.children);
            }
        };
        if self.opts.strict && self.diags.count(Severity::Error) > 0 {
            return Err(XarError::Limit("strict: an error diagnostic was raised"));
        }
        match self.emit(&mut decoded, node)? {
            After::Drop => Ok(()),
            After::Same => self.visit_children(&node.children),
            After::Child => {
                if node.children.is_empty() {
                    return Ok(());
                }
                self.push_scope()?;
                let r = self.visit_children(&node.children);
                self.pop_scope();
                r
            }
            After::Defaults => {
                self.visit_defaults(&node.children);
                Ok(())
            }
        }
    }

    /// `TAG_CURRENTATTRIBUTES`' children are the editor's *current*
    /// attributes: what the original applies to the next object the user
    /// draws (`Kernel/rechdoc.cpp:1942-1966` reads them in a "set current
    /// attribute" insert mode). They are **not** the document's default
    /// attribute block, which is fixed at start-up and never written to a
    /// file (`docs/research/02-document-model.md` §4.4). An object with no
    /// fill in the file inherits the factory default, "no colour", never the
    /// current fill; treating these as defaults filled every unfilled frame
    /// with the current colour (XARA-T-0037).
    ///
    /// The model has no current-attribute store yet (the drawing tools own
    /// it), so they are counted, compared with the defaults and skipped.
    /// Definitions inside the block (colours) are still registered: later
    /// records may reference them.
    fn visit_defaults(&mut self, children: &[RecordNode]) {
        for child in children {
            // A default is a single record; a subtree under one is never
            // visited, so it is accounted for as skipped.
            self.skipped = self.skipped.saturating_add(count_subtree(&child.children));
            let rec = &child.record;
            let at = (rec.number, rec.tag);
            let Ok(d) = decode(rec.tag, &rec.data, self.origin, &mut self.diags, at) else {
                self.skipped = self.skipped.saturating_add(1);
                continue;
            };
            if self.definition(&d, rec) {
                self.mapped = self.mapped.saturating_add(1);
                continue;
            }
            self.skipped = self.skipped.saturating_add(1);
            if let Some(v) = self.attribute(&d, at) {
                self.current_attributes = self.current_attributes.saturating_add(1);
                let differs = v
                    .slot()
                    .is_some_and(|s| **self.builder.document().defaults.get(s) != v);
                if differs {
                    self.current_differing = self.current_differing.saturating_add(1);
                }
            }
        }
    }

    // ── One record ──────────────────────────────────────────────────────────

    fn emit(&mut self, d: &mut Decoded, node: &RecordNode) -> Result<After, XarError> {
        let rec = &node.record;
        let at = (rec.number, rec.tag);

        // A definition is registered wherever it appears — including inside
        // a `TAG_CURRENTATTRIBUTES` block, where several files put the
        // colour their default fill then references.
        if self.definition(d, rec) {
            self.mapped = self.mapped.saturating_add(1);
            return Ok(After::Same);
        }

        // Attributes next: they are much the commonest thing in a file.
        if let Some(v) = self.attribute(d, at) {
            self.mapped = self.mapped.saturating_add(1);
            self.apply_attribute(v)?;
            return Ok(After::Same);
        }

        Ok(match d {
            // Structure the physical layer has already acted on.
            Decoded::Up
            | Decoded::Down
            | Decoded::EndOfFile
            | Decoded::AtomicTags(_)
            | Decoded::EssentialTags(_)
            | Decoded::StartCompression(_)
            | Decoded::EndCompression { .. }
            | Decoded::Header(_) => {
                self.mapped = self.mapped.saturating_add(1);
                After::Same
            }

            // The builder's root *is* the document node, so `TAG_DOCUMENT`
            // creates nothing and its children stay at the root level.
            Decoded::Document => {
                self.mapped = self.mapped.saturating_add(1);
                After::Same
            }
            Decoded::Chapter => {
                self.mapped = self.mapped.saturating_add(1);
                self.builder.node(NodeKind::Chapter)?;
                After::Child
            }
            Decoded::Spread => {
                self.emit_spread(node)?;
                After::Drop
            }
            Decoded::Layer => {
                self.mapped = self.mapped.saturating_add(1);
                let layer = layer_from(&node.children);
                self.builder.node(NodeKind::Layer(Box::new(layer)))?;
                After::Child
            }
            Decoded::Group => {
                self.mapped = self.mapped.saturating_add(1);
                self.builder.node(NodeKind::Group(Box::default()))?;
                After::Child
            }
            Decoded::ClipViewController => {
                self.emit_clip_view(node)?;
                After::Drop
            }
            Decoded::ShadowController(r) => {
                let r = *r;
                self.emit_shadow(node, r)?;
                After::Drop
            }
            // A shadow node outside a controller has no object to shadow.
            // Its children are its own attributes, which must not leak
            // onto the objects after it: the whole subtree goes.
            Decoded::Shadow { .. } => {
                self.mapped = self.mapped.saturating_add(1);
                self.skipped = self.skipped.saturating_add(count_subtree(&node.children));
                self.diags.push(
                    Diagnostic::new(DiagCode::ShadowDegraded)
                        .at(at.0, at.1)
                        .with_detail(1),
                );
                After::Drop
            }
            Decoded::BevelController(r) => {
                let r = *r;
                self.emit_bevel(node, r)?;
                After::Drop
            }
            // A bevel node outside a controller has nothing to bevel; its
            // children are its own attributes and go with it, as a stray
            // shadow node's do.
            Decoded::BevelInk => {
                self.mapped = self.mapped.saturating_add(1);
                self.skipped = self.skipped.saturating_add(count_subtree(&node.children));
                self.diags.push(
                    Diagnostic::new(DiagCode::BevelDegraded)
                        .at(at.0, at.1)
                        .with_detail(1),
                );
                After::Drop
            }
            Decoded::ContourController(r) => {
                let r = *r;
                self.emit_contour(node, r)?;
                After::Drop
            }
            Decoded::BlendProfiles(p) => {
                self.pending_blend_profiles = Some(*p);
                self.mapped = self.mapped.saturating_add(1);
                self.skipped = self.skipped.saturating_add(count_subtree(&node.children));
                After::Drop
            }
            Decoded::Blend(r) => {
                let r = *r;
                self.emit_blend(node, r)?;
                After::Drop
            }
            // A blender outside a blend has nothing to blend.
            Decoded::Blender { .. } => {
                self.skipped = self
                    .skipped
                    .saturating_add(count_subtree(&node.children).saturating_add(1));
                self.diags.push(
                    Diagnostic::new(DiagCode::BlendDegraded)
                        .at(at.0, at.1)
                        .with_detail(1),
                );
                After::Drop
            }
            // Read with their blend or blender; anywhere else they say
            // nothing.
            Decoded::BlenderExtra(_)
            | Decoded::BlenderCurveProp(..)
            | Decoded::BlenderCurveAngles(..)
            | Decoded::BlendPath(_)
            | Decoded::BlendPathFilled(_) => {
                self.skipped = self
                    .skipped
                    .saturating_add(count_subtree(&node.children).saturating_add(1));
                After::Drop
            }
            // A contour node outside a controller has nothing to contour;
            // its attributes go with it, as a stray bevel node's do.
            Decoded::ContourInk => {
                self.mapped = self.mapped.saturating_add(1);
                self.skipped = self.skipped.saturating_add(count_subtree(&node.children));
                self.diags.push(
                    Diagnostic::new(DiagCode::ContourDegraded)
                        .at(at.0, at.1)
                        .with_detail(1),
                );
                After::Drop
            }
            // The original reads these four and discards the value
            // (`docs/memory/xar-import.md`, finding 21).
            Decoded::BevelAttribute(_) => {
                self.mapped = self.mapped.saturating_add(1);
                After::Same
            }
            // A marker outside a controller separates nothing; its
            // children, if any, stay at this level.
            Decoded::ClipView => {
                self.mapped = self.mapped.saturating_add(1);
                self.diags.push(
                    Diagnostic::new(DiagCode::ClipViewDegraded)
                        .at(at.0, at.1)
                        .with_detail(1),
                );
                After::Same
            }

            // Records consumed by the node that encloses them.
            Decoded::SpreadInformation(_)
            | Decoded::LayerDetails { .. }
            | Decoded::LayerFrameProps { .. }
            | Decoded::SpreadAnimProps(_)
            | Decoded::PathFlags(_)
            | Decoded::TextWordWrap { .. }
            | Decoded::TextIndents { .. }
            | Decoded::GridRulerSettings { .. }
            | Decoded::GridRulerOrigin(_) => {
                self.mapped = self.mapped.saturating_add(1);
                After::Same
            }

            Decoded::Path { path, style } => {
                self.mapped = self.mapped.saturating_add(1);
                // The decoded record is dropped after this; take its path
                // rather than copy three vectors per path record.
                let mut path = std::mem::take(path);
                if let Some(flags) = path_flags_of(&node.children) {
                    apply_path_flags(&mut path, flags, &mut self.diags, at);
                }
                self.builder.node(NodeKind::Path(Box::new(PathNode {
                    data: Arc::new(path),
                    filled: style.filled,
                    stroked: style.stroked,
                })))?;
                After::Child
            }
            Decoded::RegularShape(s) => {
                self.mapped = self.mapped.saturating_add(1);
                self.builder
                    .node(NodeKind::QuickShape(Box::new(quick_shape(s))))?;
                After::Child
            }
            Decoded::NodeBitmap { corners, bitmap } => {
                match self.bitmap(*bitmap, at) {
                    Some(image) => {
                        self.mapped = self.mapped.saturating_add(1);
                        let origin = corners.first().copied().unwrap_or(Point::ORIGIN);
                        let p1 = corners.get(1).copied().unwrap_or(origin);
                        let p2 = corners.get(2).copied().unwrap_or(origin);
                        // `p0 → p1` is the width and `p1 → p2` the height
                        // (`research/01 §4.5`). Saturating, because the
                        // corners come straight out of the file.
                        let major = Vector::new(
                            p1.x.saturating_sub(origin.x),
                            p1.y.saturating_sub(origin.y),
                        );
                        let minor =
                            Vector::new(p2.x.saturating_sub(p1.x), p2.y.saturating_sub(p1.y));
                        self.builder
                            .node(NodeKind::Bitmap(Box::new(xarast_doc::BitmapNode {
                                image,
                                origin,
                                major,
                                minor,
                                photo_ops: Default::default(),
                            })))?;
                    }
                    // `opaque_node` counts the record itself; counting it
                    // here too would map it twice.
                    None => {
                        self.opaque_node(rec)?;
                    }
                }
                After::Child
            }

            Decoded::TextStory(s) => {
                if self.opts.skip_text {
                    self.skipped = self
                        .skipped
                        .saturating_add(count_subtree(std::slice::from_ref(node)));
                    return Ok(After::Drop);
                }
                self.mapped = self.mapped.saturating_add(1);
                self.builder.node(NodeKind::TextStory(Box::new(text_story(
                    s,
                    rec.tag,
                    &node.children,
                ))))?;
                self.push_scope()?;
                // The original draws a story that sets no size at its own
                // factory default, 16 pt. The model's default is the same
                // (XARA-T-0172); should they ever differ, say so explicitly
                // where the file relies on it.
                let size = self.attrs.get(AttrSlot::TxtFontSize).clone();
                if size != xarast_doc::default_for(AttrSlot::TxtFontSize) {
                    self.builder.attribute(size)?;
                }
                let r = self.visit_children(&node.children);
                self.pop_scope();
                r?;
                After::Drop
            }
            Decoded::TextLine => {
                self.mapped = self.mapped.saturating_add(1);
                self.builder.node(NodeKind::TextLine(Box::default()))?;
                After::Child
            }
            Decoded::TextString(s) => {
                self.mapped = self.mapped.saturating_add(1);
                let items: Vec<TextItem> = s.chars().map(TextItem::Char).collect();
                self.text_items(&items, node)?;
                After::Drop
            }
            Decoded::TextChar(u) => {
                self.mapped = self.mapped.saturating_add(1);
                let u = *u;
                // A character outside the BMP is written as two
                // `TAG_TEXT_CHAR` records, one UTF-16 code unit each: hold a
                // high surrogate until its low half arrives. The pair takes
                // the first record's attributes (both carry the same).
                let ch = if (0xD800..0xDC00).contains(&u) {
                    if self.pending_high.replace(u).is_some() {
                        // Two high halves in a row: the first is lone.
                        self.builder
                            .node(NodeKind::TextItem(TextItem::Char('\u{fffd}')))?;
                    }
                    self.visit_text_leftovers(node)?;
                    return Ok(After::Drop);
                } else if (0xDC00..0xE000).contains(&u) {
                    match self.pending_high.take() {
                        Some(hi) => {
                            let c = char::decode_utf16([hi, u]).next().and_then(Result::ok);
                            c.unwrap_or('\u{fffd}')
                        }
                        None => '\u{fffd}',
                    }
                } else {
                    self.flush_high()?;
                    char::from_u32(u32::from(u)).unwrap_or('\u{fffd}')
                };
                self.text_items(&[TextItem::Char(ch)], node)?;
                After::Drop
            }
            Decoded::TextEol => {
                self.mapped = self.mapped.saturating_add(1);
                self.flush_high()?;
                self.text_items(&[TextItem::LineBreak(true)], node)?;
                After::Drop
            }
            Decoded::TextTab => {
                self.mapped = self.mapped.saturating_add(1);
                self.flush_high()?;
                self.text_items(&[TextItem::Tab], node)?;
                After::Drop
            }
            Decoded::TextKern(v) => {
                self.mapped = self.mapped.saturating_add(1);
                self.flush_high()?;
                self.text_items(&[TextItem::Kern(v.dx)], node)?;
                After::Drop
            }
            // Line metrics are Phase 9's derived cache, not node data.
            Decoded::TextLineInfo { .. } => {
                self.skipped = self.skipped.saturating_add(1);
                After::Same
            }

            Decoded::DocumentDates {
                created,
                last_saved,
            } => {
                self.mapped = self.mapped.saturating_add(1);
                let mut meta = self.builder.document().meta.clone();
                meta.created = Some(i64::from(*created));
                meta.modified = Some(i64::from(*last_saved));
                self.builder.meta(meta);
                After::Same
            }

            Decoded::CurrentAttributes(_) => {
                self.mapped = self.mapped.saturating_add(1);
                After::Defaults
            }

            // Read, understood and deliberately dropped: view state,
            // printing, units, editor hints.
            Decoded::Viewport(_)
            | Decoded::DocumentView { .. }
            | Decoded::DefaultUnits { .. }
            | Decoded::DocumentUndoSize(_)
            | Decoded::DocumentFlags(_)
            | Decoded::DocumentNudge(_)
            | Decoded::BitmapSmoothing(_)
            | Decoded::DuplicationOffset(_)
            | Decoded::BarProperty(_)
            | Decoded::SetSentinel
            | Decoded::SpreadScaling { .. }
            | Decoded::Bounds(_)
            | Decoded::BitmapProperties { .. }
            | Decoded::PreviewBitmap(_) => {
                self.skipped = self.skipped.saturating_add(1);
                After::Same
            }

            Decoded::Unhandled { tag, .. } if class_of(*tag) == Some(TagClass::Ignorable) => {
                self.skipped = self.skipped.saturating_add(1);
                After::Same
            }

            // `Decoded` is `#[non_exhaustive]`; a variant added later without
            // a mapping is kept rather than silently lost.
            _ => {
                self.opaque_node(rec)?;
                After::Child
            }
        })
    }

    /// Registers a colour, bitmap or font definition. Returns whether the
    /// record was one.
    fn definition(&mut self, d: &Decoded, rec: &crate::Record) -> bool {
        match d {
            Decoded::ColourDefinition(c) => {
                let builder = &mut self.builder;
                self.colours
                    .define_external(rec.number, c, &mut self.diags, |def| {
                        builder.define_colour(def)
                    });
            }
            Decoded::BitmapDefinition(b) => {
                let bytes: &[u8] = rec.data.get(b.image.clone()).unwrap_or(&[]);
                let original = if self.opts.skip_bitmaps {
                    None
                } else {
                    Some(Arc::new(OriginalEncoded {
                        format: image_format(b.format),
                        bytes: self.bitmap_bytes(b.format, bytes, (rec.number, rec.tag)),
                    }))
                };
                let id = self.builder.define_bitmap(BitmapResource {
                    name: Arc::from(b.name.as_str()),
                    info: BitmapInfo::default(),
                    // Tag 71 (JPEG8BPP) carries the palette of the 8 bpp
                    // original; keeping it is what lets the decoder map
                    // the 24 bpp JPEG back onto it (`research/01 §4.5`).
                    // The pixels stay empty: decoding is the renderer's.
                    pixels: Arc::new(BitmapData {
                        pixels: Arc::from(&[][..]),
                        palette: b
                            .palette
                            .iter()
                            .map(|&[r, g, bl]| xarast_color::Rgba8 {
                                r,
                                g,
                                b: bl,
                                a: 255,
                            })
                            .collect(),
                    }),
                    original,
                    procedural: None,
                    transparent_index: None,
                });
                self.bitmap_by_record.insert(rec.number, id);
            }
            Decoded::FontDefinition(f) => {
                self.font_by_record.insert(
                    rec.number,
                    Arc::new(TypefaceRef {
                        full_name: Arc::from(f.full_name.as_str()),
                        family: Arc::from(f.typeface.as_str()),
                        panose: Some(f.panose),
                    }),
                );
            }
            _ => return false,
        }
        true
    }

    /// A `TAG_CLIPVIEWCONTROLLER` and everything under it.
    ///
    /// In the format the controller's ink children *before* its
    /// `TAG_CLIPVIEW` marker are the keyholes and the ones *after* it are
    /// clipped (`research/01 §4.8`). The original draws the keyholes as
    /// ordinary objects (fill and whole outline, unclipped, beneath the
    /// rest) and clips the rest to the union of the keyholes' filled
    /// areas, outlines left out. The model's ClipView takes its first child
    /// as the clipping path and never paints it, so the mapping is
    ///
    /// ```text
    /// Group                      the controller
    ///   attributes…
    ///   keyholes…                painted, as the original paints them
    ///   ClipView (Inside)
    ///     Path                   the keyholes' union, never painted
    ///     clipped objects…
    /// ```
    ///
    /// which draws what the original draws. The original has no "keep the
    /// outside" mode, so the ClipView is always `Inside`.
    fn emit_clip_view(&mut self, node: &RecordNode) -> Result<(), XarError> {
        self.mapped = self.mapped.saturating_add(1);
        let group = self.builder.node(NodeKind::Group(Box::default()))?;
        if node.children.is_empty() {
            return Ok(());
        }
        self.push_scope()?;
        let r = self.clip_view_children(node, group.node_id());
        self.pop_scope();
        r
    }

    fn clip_view_children(
        &mut self,
        node: &RecordNode,
        group: xarast_doc::NodeId,
    ) -> Result<(), XarError> {
        let rec = &node.record;
        let children = node.children.as_slice();
        let Some(marker) = children
            .iter()
            .position(|c| c.record.tag == crate::tags::TAG_CLIPVIEW)
        else {
            // No marker: the original finds no ClipView node and draws
            // the controller as the group it derives from.
            self.diags.push(
                Diagnostic::new(DiagCode::ClipViewDegraded)
                    .at(rec.number, rec.tag)
                    .with_detail(0),
            );
            return self.visit_children(children);
        };
        let (keyholes, rest) = children.split_at(marker);
        let Some((cv, clipped)) = rest.split_first() else {
            return self.visit_children(keyholes);
        };
        self.visit_children(keyholes)?;
        // The marker itself. Only the first one separates; a later one is
        // an ordinary (stray) marker among the clipped objects.
        self.mapped = self.mapped.saturating_add(1);
        let rule = match self.attrs.get(AttrSlot::WindingRule) {
            AttrValue::WindingRule(r) => *r,
            _ => FillRule::EvenOdd,
        };
        let at = (cv.record.number, cv.record.tag);
        let Some(clip) = self.keyhole_clip(group, rule, at) else {
            self.diags.push(
                Diagnostic::new(DiagCode::ClipViewDegraded)
                    .at(at.0, at.1)
                    .with_detail(3),
            );
            self.visit_children(&cv.children)?;
            return self.visit_children(clipped);
        };
        self.builder
            .node(NodeKind::ClipView(xarast_doc::ClipViewNode {
                mode: xarast_doc::ClipViewMode::Inside,
            }))?;
        self.push_scope()?;
        let r = self.clipped(clip, &cv.children, clipped);
        self.pop_scope();
        r
    }

    /// The inside of a mapped ClipView: the clipping path first, then what
    /// it clips.
    fn clipped(
        &mut self,
        clip: xarast_geom::Path,
        under_marker: &[RecordNode],
        clipped: &[RecordNode],
    ) -> Result<(), XarError> {
        self.builder.node(NodeKind::Path(Box::new(PathNode {
            data: Arc::new(clip),
            filled: true,
            stroked: false,
        })))?;
        self.visit_children(under_marker)?;
        self.visit_children(clipped)
    }

    /// The clipping path of a ClipView: the union of the filled areas of
    /// the keyholes already emitted under `group`, each under its own
    /// winding rule (`rule` is the one in force in the group). A single
    /// keyhole under the group's rule is taken verbatim; anything else is
    /// resolved by the boolean engine into non-overlapping, consistently
    /// oriented contours, which clip the same under the non-zero and
    /// even-odd rules.
    ///
    /// `None` when no keyhole has an outline the importer can compute.
    fn keyhole_clip(
        &mut self,
        group: xarast_doc::NodeId,
        rule: FillRule,
        at: (u32, u32),
    ) -> Option<xarast_geom::Path> {
        let doc = self.builder.document();
        let mut parts: Vec<(xarast_geom::Path, FillRule)> = Vec::new();
        let mut missed = false;
        for id in doc.tree.children(group) {
            match doc.tree.kind(id) {
                Some(NodeKind::Attr(_)) | None => {}
                Some(_) => {
                    let own = own_winding(doc, id).unwrap_or(rule);
                    if !silhouettes(doc, id, own, &mut parts, 0) {
                        missed = true;
                    }
                }
            }
        }
        if missed {
            self.diags.push(
                Diagnostic::new(DiagCode::ClipViewDegraded)
                    .at(at.0, at.1)
                    .with_detail(2),
            );
        }
        if let [(p, r)] = parts.as_slice()
            && *r == rule
        {
            return Some(p.clone());
        }
        let points: usize = parts.iter().map(|(p, _)| p.points().len()).sum();
        if points > MAX_KEYHOLE_POINTS {
            self.diags.push(
                Diagnostic::new(DiagCode::ClipViewDegraded)
                    .at(at.0, at.1)
                    .with_detail(4),
            );
            return parts.into_iter().next().map(|(p, _)| p);
        }
        let tol = xarast_geom::Tolerance::BOOLEAN;
        let mut acc: Option<xarast_geom::Path> = None;
        for (p, r) in &parts {
            // Negative and positive keyholes clip as the original draws
            // them, non-zero (`FillRule::as_drawn`).
            let resolved = xarast_geom::self_union(p, r.as_drawn(), tol);
            acc = Some(match acc {
                None => resolved,
                Some(a) => xarast_geom::boolean(
                    &a,
                    &resolved,
                    xarast_geom::BoolOp::Union,
                    FillRule::NonZero,
                    tol,
                ),
            });
        }
        acc
    }

    /// A `TAG_SHADOWCONTROLLER` and everything under it.
    ///
    /// In the file the controller holds its attributes, one `TAG_SHADOW`
    /// (whose own attribute children are the shadow's fill: its colour, and
    /// a flat transparency that repeats the darkness) and the object that
    /// casts the shadow. The model's live object is
    ///
    /// ```text
    /// Live (Controller, Shadow)   parameters of both records, the colour
    ///   attributes…               the controller's: shadow and object
    ///   Live (Generated, Shadow)  empty: the renderer draws the shadow
    ///   Live (Source, Shadow)
    ///     the object…
    /// ```
    ///
    /// The shadow's own attributes are folded into the parameters rather
    /// than kept as nodes: the colour is the only one that means anything
    /// (the original paints the shadow through a bitmap transparency built
    /// from the darkness, whatever transparency the node carries), and a
    /// parameter survives a `.xarast` round trip where an attribute of a
    /// node with no ink would not.
    fn emit_shadow(
        &mut self,
        node: &RecordNode,
        r: crate::decode::ShadowControllerRecord,
    ) -> Result<(), XarError> {
        let rec = &node.record;
        let at = (rec.number, rec.tag);
        self.mapped = self.mapped.saturating_add(1);
        let kind = match r.kind {
            1 => xarast_doc::ShadowKind::Wall,
            2 => xarast_doc::ShadowKind::Floor,
            3 => xarast_doc::ShadowKind::Glow,
            other => {
                self.diags.push(
                    Diagnostic::new(DiagCode::ShadowDegraded)
                        .at(at.0, at.1)
                        .with_detail(3),
                );
                self.diags.push(
                    Diagnostic::new(DiagCode::UnknownEnumValue)
                        .at(at.0, at.1)
                        .with_detail(u64::from(other)),
                );
                xarast_doc::ShadowKind::Wall
            }
        };
        let angle = f64::from(r.floor_angle) / 1_000_000.0 % std::f64::consts::TAU;
        let mut params = xarast_doc::ShadowParams {
            kind,
            offset: Point::raw(r.offset.0, r.offset.1),
            blur: Mp::new(r.penumbra),
            darkness: 1.0,
            profile: BiasGain::IDENTITY,
            // f32-ok: a percentage over 100 and an angle within ±2π.
            scale: (f64::from(r.floor_height) / 100.0) as f32,
            tilt: angle as f32,
            glow_width: Mp::new(r.width),
            colour: Colour::Direct(ColourValue::BLACK),
        };
        let children = node.children.as_slice();
        let shadow_at = children
            .iter()
            .position(|c| c.record.tag == crate::tags::TAG_SHADOW);
        match shadow_at.and_then(|i| children.get(i)) {
            Some(s) => self.shadow_node(s, &mut params),
            None => self.diags.push(
                Diagnostic::new(DiagCode::ShadowDegraded)
                    .at(at.0, at.1)
                    .with_detail(0),
            ),
        }
        let live = |role| {
            NodeKind::Live(Box::new(xarast_doc::LiveNode {
                role,
                kind: xarast_doc::LiveKind::Shadow(Box::new(params.clone())),
                regen: xarast_doc::RegenState::Clean,
                name: None,
            }))
        };
        self.builder.node(live(xarast_doc::LiveRole::Controller))?;
        self.push_scope()?;
        let r = self.live_children(children, shadow_at, &live);
        self.pop_scope();
        r
    }

    /// The controller's children: its attributes (everything before the
    /// first object, which the generated node and the object both
    /// inherit), then the generated node, then the source holding the
    /// rest. `generated_at` is the file's generated node (a `TAG_SHADOW`
    /// or a `TAG_BEVELINK`), folded into the parameters by the caller; a
    /// stray second one is dropped with its attributes. Shared by shadows
    /// and bevels.
    fn live_children(
        &mut self,
        children: &[RecordNode],
        generated_at: Option<usize>,
        live: &dyn Fn(xarast_doc::LiveRole) -> NodeKind,
    ) -> Result<(), XarError> {
        let first_ink = children
            .iter()
            .enumerate()
            .position(|(i, c)| {
                Some(i) != generated_at && class_of(c.record.tag) == Some(TagClass::Object)
            })
            .unwrap_or(children.len());
        for (i, c) in children.iter().enumerate().take(first_ink) {
            if Some(i) != generated_at {
                self.visit(c)?;
            }
        }
        self.builder.node(live(xarast_doc::LiveRole::Generated))?;
        self.builder.node(live(xarast_doc::LiveRole::Source))?;
        self.push_scope()?;
        let mut r = Ok(());
        for (i, c) in children.iter().enumerate().skip(first_ink) {
            if Some(i) == generated_at {
                continue;
            }
            r = self.visit(c);
            if r.is_err() {
                break;
            }
        }
        self.pop_scope();
        r
    }

    /// A `TAG_BEVEL` and everything under it.
    ///
    /// In the file the controller holds its attributes, the object(s) it
    /// bevels and one `TAG_BEVELINK` (first for an outer bevel, last for an
    /// inner one), whose own attribute children are copies of the first
    /// object's fill. The model's live object is
    ///
    /// ```text
    /// Live (Controller, Bevel)   the record's parameters
    ///   attributes…              the controller's, before the first object
    ///   Live (Generated, Bevel)  empty: the renderer draws the bevel
    ///   Live (Source, Bevel)
    ///     the object(s)…
    /// ```
    ///
    /// The bevel node's flat fill becomes [`xarast_doc::BevelParams::colour`]
    /// (what an outer bevel is painted in); the rest of its attributes
    /// repeat the object's and are dropped (`docs/memory/xar-import.md`,
    /// finding 21).
    fn emit_bevel(
        &mut self,
        node: &RecordNode,
        r: crate::decode::BevelRecord,
    ) -> Result<(), XarError> {
        let rec = &node.record;
        let at = (rec.number, rec.tag);
        self.mapped = self.mapped.saturating_add(1);
        let bevel_type = match xarast_doc::BevelType::from_index(i64::from(r.style)) {
            Some(t) => t,
            None => {
                self.diags.push(
                    Diagnostic::new(DiagCode::BevelDegraded)
                        .at(at.0, at.1)
                        .with_detail(3),
                );
                self.diags.push(
                    Diagnostic::new(DiagCode::UnknownEnumValue)
                        .at(at.0, at.1)
                        .with_detail(u64::from(r.style.unsigned_abs())),
                );
                xarast_doc::BevelType::Flat
            }
        };
        // f32-ok: whole degrees and a percentage, far inside f32's exact
        // integers; the contrast is a percentage over 100.
        let mut params = xarast_doc::BevelParams {
            bevel_type,
            indent: Mp::new(r.indent),
            outer: r.outer != 0,
            light_angle: r.light_angle as f32,
            light_tilt: r.tilt as f32,
            contrast: (f64::from(r.contrast) / 100.0) as f32,
            colour: None,
        };
        let children = node.children.as_slice();
        let ink_at = children
            .iter()
            .position(|c| c.record.tag == crate::tags::TAG_BEVELINK);
        match ink_at.and_then(|i| children.get(i)) {
            Some(ink) => self.bevel_ink(ink, &mut params),
            None => self.diags.push(
                Diagnostic::new(DiagCode::BevelDegraded)
                    .at(at.0, at.1)
                    .with_detail(0),
            ),
        }
        let live = |role| {
            NodeKind::Live(Box::new(xarast_doc::LiveNode {
                role,
                kind: xarast_doc::LiveKind::Bevel(Box::new(params.clone())),
                regen: xarast_doc::RegenState::Clean,
                name: None,
            }))
        };
        self.builder.node(live(xarast_doc::LiveRole::Controller))?;
        self.push_scope()?;
        let r = self.live_children(children, ink_at, &live);
        self.pop_scope();
        r
    }

    /// A `TAG_CONTOURCONTROLLER` and everything under it.
    ///
    /// In the file the controller holds its attributes, the object(s) it
    /// contours and one `TAG_CONTOUR`: first for an outer contour (its
    /// rings are drawn beneath the objects), last for an inner one. The
    /// contour node's own attribute children are what its last ring is
    /// drawn in, and the start of the blend towards the first object's
    /// (`xarast_doc::contour`). The model keeps that order:
    ///
    /// ```text
    /// Live (Controller, Contour)   the record's parameters
    ///   attributes…                the controller's, before the first object
    ///   Live (Generated, Contour)  the contour node's attributes (outer)
    ///   Live (Source, Contour)
    ///     the object(s)…
    ///   Live (Generated, Contour)  … or here, after them (inner)
    /// ```
    ///
    /// The record's width is negative for an outer contour
    /// (`Kernel/ncntrcnt.h:255`); the model keeps its magnitude and the
    /// direction apart (`docs/memory/xar-import.md`, finding 22).
    fn emit_contour(
        &mut self,
        node: &RecordNode,
        r: crate::decode::ContourRecord,
    ) -> Result<(), XarError> {
        let rec = &node.record;
        let at = (rec.number, rec.tag);
        self.mapped = self.mapped.saturating_add(1);
        let max_steps = xarast_doc::live::MAX_CONTOUR_RINGS.saturating_sub(1);
        let steps = match usize::try_from(r.steps) {
            Ok(s) if s <= max_steps => s,
            _ => {
                self.diags.push(
                    Diagnostic::new(DiagCode::ContourDegraded)
                        .at(at.0, at.1)
                        .with_detail(3),
                );
                if r.steps < 0 { 0 } else { max_steps }
            }
        };
        let colour_effect = match r.kind & 0x7f {
            0 => xarast_color::FillEffect::Fade,
            1 => xarast_color::FillEffect::Rainbow,
            2 => xarast_color::FillEffect::AltRainbow,
            other => {
                self.diags.push(
                    Diagnostic::new(DiagCode::ContourDegraded)
                        .at(at.0, at.1)
                        .with_detail(2),
                );
                self.diags.push(
                    Diagnostic::new(DiagCode::UnknownEnumValue)
                        .at(at.0, at.1)
                        .with_detail(u64::from(other)),
                );
                xarast_color::FillEffect::Fade
            }
        };
        let params = xarast_doc::ContourParams {
            steps: u32::try_from(steps).unwrap_or(0),
            width: Mp::new(r.width.saturating_abs()),
            outer: r.width < 0,
            include_line_widths: false,
            join: None,
            profile: BiasGain::new(r.object_profile.0, r.object_profile.1),
            colour_profile: BiasGain::new(r.attr_profile.0, r.attr_profile.1),
            colour_effect,
            inset: r.kind & 0x80 != 0,
        };
        let children = node.children.as_slice();
        let ink_at = children
            .iter()
            .position(|c| c.record.tag == crate::tags::TAG_CONTOUR);
        if ink_at.is_none() {
            self.diags.push(
                Diagnostic::new(DiagCode::ContourDegraded)
                    .at(at.0, at.1)
                    .with_detail(0),
            );
        }
        let live = |role| {
            NodeKind::Live(Box::new(xarast_doc::LiveNode {
                role,
                kind: xarast_doc::LiveKind::Contour(Box::new(params.clone())),
                regen: xarast_doc::RegenState::Clean,
                name: None,
            }))
        };
        self.builder.node(live(xarast_doc::LiveRole::Controller))?;
        self.push_scope()?;
        let first_ink = children
            .iter()
            .enumerate()
            .position(|(i, c)| {
                Some(i) != ink_at && class_of(c.record.tag) == Some(TagClass::Object)
            })
            .unwrap_or(children.len());
        let mut r = Ok(());
        for (i, c) in children.iter().enumerate().take(first_ink) {
            if Some(i) != ink_at {
                r = self.visit(c);
                if r.is_err() {
                    break;
                }
            }
        }
        // The contour node goes where the file put it relative to the
        // objects; without one, where its direction would put it.
        let generated_first = ink_at.map_or(params.outer, |i| i < first_ink);
        let ink = ink_at.and_then(|i| children.get(i));
        if r.is_ok() && generated_first {
            r = self.contour_ink(ink, &live);
        }
        if r.is_ok() {
            self.builder.node(live(xarast_doc::LiveRole::Source))?;
            self.push_scope()?;
            for (i, c) in children.iter().enumerate().skip(first_ink) {
                if Some(i) == ink_at {
                    continue;
                }
                r = self.visit(c);
                if r.is_err() {
                    break;
                }
            }
            self.pop_scope();
        }
        if r.is_ok() && !generated_first {
            r = self.contour_ink(ink, &live);
        }
        self.pop_scope();
        r
    }

    /// The generated contour node, holding the `TAG_CONTOUR`'s attribute
    /// children (none without one).
    fn contour_ink(
        &mut self,
        ink: Option<&RecordNode>,
        live: &dyn Fn(xarast_doc::LiveRole) -> NodeKind,
    ) -> Result<(), XarError> {
        self.builder.node(live(xarast_doc::LiveRole::Generated))?;
        let Some(ink) = ink else {
            return Ok(());
        };
        self.mapped = self.mapped.saturating_add(1);
        self.push_scope()?;
        let mut r = Ok(());
        for c in &ink.children {
            r = self.visit(c);
            if r.is_err() {
                break;
            }
        }
        self.pop_scope();
        r
    }

    /// A `TAG_BLEND` and everything under it.
    ///
    /// In the file the blend holds its attributes, then its objects with a
    /// `TAG_BLENDER` (and its `TAG_BLENDERADDITIONAL`) between each pair
    /// (`research/02 §6.6`). The model keeps that order, the blenders as
    /// generated nodes under the source:
    ///
    /// ```text
    /// Live (Controller, Blend)   the record's parameters, every blender's
    ///   attributes…              the blend's, before the first object
    ///   Live (Source, Blend)
    ///     object 0
    ///     Live (Generated, Blend)  steps from object 0 to object 1
    ///     object 1 …
    /// ```
    ///
    /// The flags and the profiles are as the original writes them
    /// (`Kernel/nodeblnd.cpp:3525-3560`; `docs/memory/xar-import.md`,
    /// finding 23).
    fn emit_blend(
        &mut self,
        node: &RecordNode,
        r: crate::decode::BlendRecord,
    ) -> Result<(), XarError> {
        use crate::tags::{
            TAG_BLEND_PATH, TAG_BLENDER, TAG_BLENDERADDITIONAL, TAG_NODEBLENDPATH_FILLED,
        };
        let rec = &node.record;
        let at = (rec.number, rec.tag);
        self.mapped = self.mapped.saturating_add(1);
        let profiles = self.pending_blend_profiles.take().unwrap_or_default();
        let degraded = |me: &mut Self, detail: u64| {
            me.diags.push(
                Diagnostic::new(DiagCode::BlendDegraded)
                    .at(at.0, at.1)
                    .with_detail(detail),
            );
        };
        let colour_effect = match r.flags >> 4 {
            0 => Some(xarast_color::FillEffect::Fade),
            1 => Some(xarast_color::FillEffect::Rainbow),
            2 => Some(xarast_color::FillEffect::AltRainbow),
            3 => None,
            other => {
                degraded(self, 2);
                self.diags.push(
                    Diagnostic::new(DiagCode::UnknownEnumValue)
                        .at(at.0, at.1)
                        .with_detail(u64::from(other)),
                );
                Some(xarast_color::FillEffect::Fade)
            }
        };
        if u32::from(r.steps) > xarast_doc::live::MAX_BLEND_STEPS {
            degraded(self, 5);
        }
        let children = node.children.as_slice();
        // What the original counts as an object index: every ink child,
        // blenders included, blend paths not (`Kernel/nodebldr.cpp:3188-
        // 3240`).
        let is_object = |c: &RecordNode| {
            c.record.tag != TAG_BLEND_PATH && class_of(c.record.tag) == Some(TagClass::Object)
        };
        let mut blenders = Vec::new();
        let mut along_path = false;
        let mut index = 0i32;
        for (i, c) in children.iter().enumerate() {
            let tag = c.record.tag;
            if tag == TAG_BLEND_PATH || tag == TAG_NODEBLENDPATH_FILLED {
                along_path = true;
            }
            if tag == TAG_BLENDER {
                let (path_start, path_end) = match decode(
                    tag,
                    &c.record.data,
                    self.origin,
                    &mut self.diags,
                    (c.record.number, tag),
                ) {
                    Ok(Decoded::Blender {
                        path_start,
                        path_end,
                    }) => (path_start, path_end),
                    _ => (-1, -1),
                };
                let index_of = |i: i32| u32::try_from(i).ok();
                blenders.push(xarast_doc::BlenderParams {
                    start_index: index_of(path_start),
                    end_index: index_of(path_end),
                    pairs: Vec::new(),
                });
                if !c.children.is_empty() {
                    along_path = true;
                }
                let extra = children
                    .get(i.saturating_add(1))
                    .filter(|n| n.record.tag == TAG_BLENDERADDITIONAL)
                    .and_then(|n| {
                        decode(
                            n.record.tag,
                            &n.record.data,
                            self.origin,
                            &mut self.diags,
                            (n.record.number, n.record.tag),
                        )
                        .ok()
                    });
                if let Some(Decoded::BlenderExtra(x)) = extra {
                    if x.on_curve != 0 {
                        along_path = true;
                    }
                    if x.obj_start != index.saturating_sub(1)
                        || x.obj_end != index.saturating_add(1)
                    {
                        degraded(self, 4);
                    }
                }
            }
            if is_object(c) {
                index = index.saturating_add(1);
            }
        }
        if along_path {
            degraded(self, 3);
        }
        let params = xarast_doc::BlendParams {
            steps: u32::from(r.steps),
            step_distance: None,
            one_to_one: r.flags & 1 != 0,
            antialias: r.flags & 2 != 0,
            tangential: r.flags & 4 != 0,
            reverse: false,
            profile: BiasGain::new(profiles[0].0, profiles[0].1),
            attr_profile: BiasGain::new(profiles[1].0, profiles[1].1),
            colour_effect,
            blenders,
        };
        let live = |role| {
            NodeKind::Live(Box::new(xarast_doc::LiveNode {
                role,
                kind: xarast_doc::LiveKind::Blend(Box::new(params.clone())),
                regen: xarast_doc::RegenState::Clean,
                name: None,
            }))
        };
        self.builder.node(live(xarast_doc::LiveRole::Controller))?;
        self.push_scope()?;
        let first = children
            .iter()
            .position(is_object)
            .unwrap_or(children.len());
        let mut r = Ok(());
        for c in children.iter().take(first) {
            r = self.visit(c);
            if r.is_err() {
                break;
            }
        }
        if r.is_ok() {
            self.builder.node(live(xarast_doc::LiveRole::Source))?;
            self.push_scope()?;
            let mut previous_object = false;
            let mut awaiting_object = false;
            for c in children.iter().skip(first) {
                let tag = c.record.tag;
                if tag == TAG_BLENDER {
                    self.mapped = self.mapped.saturating_add(1);
                    self.skipped = self.skipped.saturating_add(count_subtree(&c.children));
                    if !previous_object {
                        degraded(self, 0);
                    }
                    self.builder.node(live(xarast_doc::LiveRole::Generated))?;
                    previous_object = false;
                    awaiting_object = true;
                    continue;
                }
                if tag == TAG_BLENDERADDITIONAL {
                    self.mapped = self.mapped.saturating_add(1);
                    continue;
                }
                if tag == TAG_BLEND_PATH || tag == TAG_NODEBLENDPATH_FILLED {
                    self.skipped = self
                        .skipped
                        .saturating_add(count_subtree(&c.children).saturating_add(1));
                    continue;
                }
                if is_object(c) {
                    previous_object = true;
                    awaiting_object = false;
                }
                r = self.visit(c);
                if r.is_err() {
                    break;
                }
            }
            if awaiting_object {
                degraded(self, 0);
            }
            self.pop_scope();
        }
        self.pop_scope();
        r
    }

    /// `TAG_BEVELINK`'s attribute children: a flat fill becomes the
    /// bevel's colour; the rest repeat the object's and are dropped.
    fn bevel_ink(&mut self, node: &RecordNode, params: &mut xarast_doc::BevelParams) {
        self.mapped = self.mapped.saturating_add(1);
        for child in &node.children {
            let crec = &child.record;
            let cat = (crec.number, crec.tag);
            self.skipped = self.skipped.saturating_add(count_subtree(&child.children));
            let Ok(d) = decode(crec.tag, &crec.data, self.origin, &mut self.diags, cat) else {
                self.skipped = self.skipped.saturating_add(1);
                continue;
            };
            if self.definition(&d, crec) {
                self.mapped = self.mapped.saturating_add(1);
                continue;
            }
            match self.attribute(&d, cat) {
                Some(AttrValue::Fill(paint)) if params.colour.is_none() => {
                    self.mapped = self.mapped.saturating_add(1);
                    if let Paint::Flat { value } = &paint {
                        params.colour = Some(value.clone());
                    } else {
                        let mut first = None;
                        let mut p = paint.clone();
                        p.for_each_value_mut(&mut |c| {
                            if first.is_none() {
                                first = Some(c.clone());
                            }
                        });
                        params.colour = first;
                        self.diags.push(
                            Diagnostic::new(DiagCode::BevelDegraded)
                                .at(cat.0, cat.1)
                                .with_detail(2),
                        );
                    }
                }
                Some(_) => self.mapped = self.mapped.saturating_add(1),
                None => self.skipped = self.skipped.saturating_add(1),
            }
        }
    }

    /// `TAG_SHADOW` and its attribute children, folded into `params`.
    fn shadow_node(&mut self, node: &RecordNode, params: &mut xarast_doc::ShadowParams) {
        let rec = &node.record;
        let at = (rec.number, rec.tag);
        self.mapped = self.mapped.saturating_add(1);
        if let Ok(Decoded::Shadow {
            bias,
            gain,
            darkness,
        }) = decode(rec.tag, &rec.data, self.origin, &mut self.diags, at)
        {
            params.profile = BiasGain::new(bias, gain);
            // The original clamps it into 0..=1 as it reads it.
            let d = if darkness.is_nan() {
                1.0
            } else {
                darkness.clamp(0.0, 1.0)
            };
            // f32-ok: a fraction in 0..=1.
            params.darkness = d as f32;
        }
        let mut coloured = false;
        for child in &node.children {
            let crec = &child.record;
            let cat = (crec.number, crec.tag);
            // Nothing nests under a shadow's attributes in any real file;
            // what a corrupt one puts there is not visited.
            self.skipped = self.skipped.saturating_add(count_subtree(&child.children));
            let Ok(d) = decode(crec.tag, &crec.data, self.origin, &mut self.diags, cat) else {
                self.skipped = self.skipped.saturating_add(1);
                continue;
            };
            if self.definition(&d, crec) {
                self.mapped = self.mapped.saturating_add(1);
                continue;
            }
            match self.attribute(&d, cat) {
                Some(AttrValue::Fill(paint)) if !coloured => {
                    self.mapped = self.mapped.saturating_add(1);
                    coloured = true;
                    if let Paint::Flat { value } = &paint {
                        params.colour = value.clone();
                    } else {
                        let mut first = None;
                        let mut p = paint.clone();
                        p.for_each_value_mut(&mut |c| {
                            if first.is_none() {
                                first = Some(c.clone());
                            }
                        });
                        if let Some(c) = first {
                            params.colour = c;
                        }
                        self.diags.push(
                            Diagnostic::new(DiagCode::ShadowDegraded)
                                .at(cat.0, cat.1)
                                .with_detail(2),
                        );
                    }
                }
                // The transparency repeats the darkness; the rest (a
                // mapping, an effect) shapes nothing on a flat colour.
                Some(_) => self.mapped = self.mapped.saturating_add(1),
                None => self.skipped = self.skipped.saturating_add(1),
            }
        }
    }

    fn opaque_node(&mut self, rec: &crate::Record) -> Result<(), XarError> {
        self.opaque = self.opaque.saturating_add(1);
        self.mapped = self.mapped.saturating_add(1);
        self.builder.node(NodeKind::Opaque(Box::new(OpaqueNode {
            tag: rec.tag,
            payload: Arc::from(rec.data.as_slice()),
        })))?;
        Ok(())
    }

    // ── Structure ───────────────────────────────────────────────────────────

    fn emit_spread(&mut self, node: &RecordNode) -> Result<(), XarError> {
        self.mapped = self.mapped.saturating_add(1);
        let info = spread_information_of(&node.children);
        let anim = anim_of(&node.children);
        let rect = info.as_ref().map_or_else(
            || SpreadNode::default().page_size,
            |i| {
                self.origin = spread_origin(i);
                pages_rect(i)
            },
        );
        let spread = SpreadNode {
            page_size: rect,
            margin: info.as_ref().map_or(Mp::ZERO, page_margin),
            bleed: info.as_ref().map_or(Mp::ZERO, |i| i.bleed),
            double_page: info.is_some_and(|i| i.double_page_spread()),
            show_shadow: info.is_some_and(|i| i.show_page_shadow()),
            anim,
        };
        let double = spread.double_page;
        self.builder.node(NodeKind::Spread(Box::new(spread)))?;
        if node.children.is_empty() {
            return Ok(());
        }
        self.push_scope()?;
        // `.xar` has no page record: the pages are implied by the spread's
        // size and its double-page flag.
        if double {
            let mid = rect.lo.x.midpoint(rect.hi.x);
            self.builder.node(NodeKind::Page(Box::new(PageNode {
                rect: Rect::new(rect.lo, Point::new(mid, rect.hi.y)),
                right_hand: false,
            })))?;
            self.builder.node(NodeKind::Page(Box::new(PageNode {
                rect: Rect::new(Point::new(mid, rect.lo.y), rect.hi),
                right_hand: true,
            })))?;
        } else {
            self.builder.node(NodeKind::Page(Box::new(PageNode {
                rect,
                right_hand: false,
            })))?;
        }
        let r = self.visit_children(&node.children);
        self.pop_scope();
        r
    }

    fn emit_grid(&mut self, siblings: &[RecordNode]) -> Result<(), XarError> {
        let mut grid = GridNode::default();
        for s in siblings {
            let rec = &s.record;
            if !matches!(rec.tag, 46 | 47) {
                continue;
            }
            let at = (rec.number, rec.tag);
            match decode(rec.tag, &rec.data, self.origin, &mut self.diags, at) {
                Ok(Decoded::GridRulerSettings {
                    unit,
                    divisions,
                    subdivisions,
                    kind,
                }) => {
                    grid.kind = if kind == 1 {
                        GridKind::Isometric
                    } else {
                        GridKind::Rect
                    };
                    // `divisions` is divisions **per unit**, and the unit is
                    // the record's own reference; the model stores the
                    // spacing of one division, in millipoints. Reading the
                    // unit matters: the corpus's grids are in pixels, where
                    // assuming points would make every grid a third too
                    // coarse.
                    let per_unit = match unit {
                        Ref::Builtin(v) => builtin_unit_mp(v),
                        _ => None,
                    };
                    if let Some(per_unit) = per_unit
                        && divisions.is_finite()
                        && divisions > 0.0
                    {
                        grid.spacing = Mp::from_f64_round(per_unit / divisions);
                    }
                    grid.subdivisions = subdivisions.min(1024);
                }
                Ok(Decoded::GridRulerOrigin(p)) => grid.origin = p,
                _ => {}
            }
        }
        self.builder.node(NodeKind::Grid(Box::new(grid)))?;
        Ok(())
    }

    // ── Attributes ──────────────────────────────────────────────────────────

    /// The [`AttrValue`] a record carries, if it carries one.
    ///
    /// The tag-to-slot table lives in `xarast_doc::attr::tags`; this function
    /// produces the *value*, and a test checks the two agree on every tag the
    /// corpus contains.
    fn attribute(&mut self, d: &Decoded, at: (u32, u32)) -> Option<AttrValue> {
        Some(match d {
            Decoded::FlatFill(r) => {
                let c = self.colour(*r, at);
                AttrValue::Fill(Paint::Flat { value: c })
            }
            Decoded::LineColour(r) => {
                let c = self.colour(*r, at);
                AttrValue::StrokeColour(Paint::Flat { value: c })
            }
            Decoded::LineWidth(w) => AttrValue::LineWidth(*w),
            Decoded::Gradient(g) => AttrValue::Fill(self.paint(g, at)),
            Decoded::FlatTransparency { level, mode } => AttrValue::TranspFill(TranspPaint::Flat {
                value: Transparency {
                    level: *level,
                    mode: *mode,
                },
            }),
            Decoded::GradientTransparency(g) => AttrValue::TranspFill(self.transp_paint(g, at)),
            Decoded::LineTransparency { level, mode } => {
                AttrValue::StrokeTransp(TranspPaint::Flat {
                    value: Transparency {
                        level: *level,
                        mode: *mode,
                    },
                })
            }
            // The format stores the two caps separately and can express a
            // mismatched pair; the model, like the original, has one cap.
            Decoded::StartCap(v) | Decoded::EndCap(v) => AttrValue::LineCap(cap(*v)),
            Decoded::JoinStyle(v) => AttrValue::JoinType(join(*v)),
            Decoded::MitreLimit(v) => AttrValue::MitreLimit(*v),
            Decoded::WindingRule(v) => AttrValue::WindingRule(FillRule::from_byte(*v)),
            Decoded::Quality(v) => AttrValue::Quality(quality(*v)),
            Decoded::FillMapping(r) => AttrValue::FillMapping(tiling(*r)),
            Decoded::TransparencyMapping(r) => AttrValue::TranspFillMapping(tiling(*r)),
            Decoded::FillEffect(e) => AttrValue::FillEffect(match e {
                FillEffect::Fade => xarast_color::FillEffect::Fade,
                FillEffect::Rainbow => xarast_color::FillEffect::Rainbow,
                FillEffect::AltRainbow => xarast_color::FillEffect::AltRainbow,
            }),
            Decoded::DashStyle(r) => AttrValue::DashPattern(Arc::new(self.dash(*r, at)?)),
            Decoded::DefineDash {
                start,
                width,
                elements,
                scaled,
            } => AttrValue::DashPattern(Arc::new(DashPattern {
                elements: elements.clone(),
                offset: *start,
                reference_width: scaled.then_some(*width),
            })),
            Decoded::Arrow {
                head,
                width,
                height,
                ..
            } => {
                let spec = Arc::new(ArrowSpec {
                    name: None,
                    path: None,
                    width: *width as f32,
                    height: *height as f32,
                });
                if *head {
                    AttrValue::StartArrow(spec)
                } else {
                    AttrValue::EndArrow(spec)
                }
            }
            Decoded::Feather { size, bias, gain } => AttrValue::Feather {
                size: *size,
                profile: BiasGain::new(*bias, *gain),
            },
            Decoded::TextAttr(a) => self.text_attribute(*a, at)?,
            _ => return None,
        })
    }

    fn text_attribute(&mut self, a: TextAttr, at: (u32, u32)) -> Option<AttrValue> {
        use xarast_doc::{Justification, LineSpacing, Script};
        Some(match a {
            TextAttr::LineSpaceRatio(v) => AttrValue::LineSpace(LineSpacing::Ratio(v as f32)),
            TextAttr::LineSpaceAbsolute(v) => AttrValue::LineSpace(LineSpacing::Absolute(v)),
            TextAttr::Justification(v) => AttrValue::Justification(match v {
                1 => Justification::Centre,
                2 => Justification::Right,
                3 => Justification::Full,
                _ => Justification::Left,
            }),
            TextAttr::FontSize(v) => AttrValue::FontSize(v),
            TextAttr::Typeface(r) => AttrValue::FontTypeface(self.typeface(r, at)),
            TextAttr::Bold(v) => AttrValue::Bold(v),
            TextAttr::Italic(v) => AttrValue::Italic(v),
            TextAttr::Underline(v) => AttrValue::Underline(v),
            TextAttr::Script { offset, size } => AttrValue::Script(Script {
                on: true,
                offset: offset as f32,
                size: size as f32,
            }),
            TextAttr::ScriptOff => AttrValue::Script(Script::default()),
            // The implicit forms; the original picks the same ratios.
            TextAttr::Superscript => AttrValue::Script(Script {
                on: true,
                offset: 0.33,
                size: 0.6,
            }),
            TextAttr::Subscript => AttrValue::Script(Script {
                on: true,
                offset: -0.15,
                size: 0.6,
            }),
            // Carried through raw: the value is thousandths of an em
            // (`Kernel/nodetext.cpp:1781-1792`), not millipoints, whatever
            // the `Mp` type says. Layout converts it (`docs/memory/text.md`).
            TextAttr::Tracking(v) => AttrValue::Tracking(Mp::new(v)),
            TextAttr::AspectRatio(v) => AttrValue::AspectRatio(v as f32),
            TextAttr::Baseline(v) => AttrValue::Baseline(v),
            TextAttr::LeftIndent(v) => AttrValue::LeftMargin(v),
            TextAttr::FirstIndent(v) => AttrValue::FirstIndent(v),
            TextAttr::RightIndent(v) => AttrValue::RightMargin(v),
        })
    }

    fn typeface(&mut self, r: Ref, at: (u32, u32)) -> Arc<TypefaceRef> {
        if let Ref::Record(n) = r
            && let Some(f) = self.font_by_record.get(&n)
        {
            return Arc::clone(f);
        }
        if r != Ref::None {
            self.diags.push(
                Diagnostic::new(DiagCode::DanglingReference)
                    .at(at.0, at.1)
                    .with_detail(match r {
                        Ref::Record(n) => u64::from(n),
                        Ref::Builtin(v) => u64::from(v.unsigned_abs()),
                        Ref::None => 0,
                    }),
            );
        }
        Arc::new(TypefaceRef {
            full_name: Arc::from("Times New Roman"),
            family: Arc::from("Times New Roman"),
            panose: None,
        })
    }

    /// The predefined dash patterns of `Kernel/cxfdash.h` are numbers, not
    /// geometry: their element lists live in the original's dash gallery and
    /// are not in the format. Rather than invent twenty patterns, an
    /// unresolvable reference produces no attribute at all, so the line keeps
    /// whatever it inherited, and says so.
    fn dash(&mut self, r: Ref, at: (u32, u32)) -> Option<DashPattern> {
        match r {
            // −21 is "solid", which is exactly the empty pattern.
            Ref::None | Ref::Builtin(-21) => Some(DashPattern::default()),
            Ref::Builtin(v) => {
                self.diags.push(
                    Diagnostic::new(DiagCode::UnknownEnumValue)
                        .at(at.0, at.1)
                        .with_detail(u64::from(v.unsigned_abs())),
                );
                None
            }
            Ref::Record(n) => {
                self.diags.push(
                    Diagnostic::new(DiagCode::DanglingReference)
                        .at(at.0, at.1)
                        .with_detail(u64::from(n)),
                );
                None
            }
        }
    }

    /// The bytes a bitmap definition's resource keeps.
    ///
    /// Verbatim, except for a PNG under tag 68 with an alpha channel: the
    /// original stores *transparency* in that channel (0 = opaque;
    /// `research/01 §4.5`), so it is rewritten, losslessly, as a standard
    /// PNG. The `.xar` convention stays at the `.xar` boundary, and the
    /// document, the renderer, a `.xarast` package and a browser all read the
    /// same bytes the same way. A file the rewrite cannot decode is kept as
    /// it is, with a warning; the renderer will count it as failed.
    fn bitmap_bytes(
        &mut self,
        format: crate::decode::BitmapFormat,
        bytes: &[u8],
        at: (u32, u32),
    ) -> Arc<[u8]> {
        if format != crate::decode::BitmapFormat::Png {
            return Arc::from(bytes);
        }
        match xarast_image::xar::normalise_xar_png(bytes, &self.opts.bitmap_limits) {
            Ok(Some(fixed)) => Arc::from(fixed),
            Ok(None) => Arc::from(bytes),
            Err(_) => {
                self.diags.push(
                    Diagnostic::new(DiagCode::BitmapNotNormalised)
                        .at(at.0, at.1)
                        .with_detail(bytes.len() as u64),
                );
                Arc::from(bytes)
            }
        }
    }

    fn bitmap(&mut self, r: Ref, at: (u32, u32)) -> Option<BitmapId> {
        match r {
            Ref::Record(n) => {
                let found = self.bitmap_by_record.get(&n).copied();
                if found.is_none() {
                    self.diags.push(
                        Diagnostic::new(DiagCode::DanglingReference)
                            .at(at.0, at.1)
                            .with_detail(u64::from(n)),
                    );
                }
                found
            }
            _ => None,
        }
    }

    /// A colour reference as a paintable value.
    ///
    /// "No colour" is the fully transparent one: the model's [`Paint`] is
    /// always a colour, and `TAG_FLATFILL_NONE` means *do not paint*, which a
    /// transparent flat fill expresses exactly.
    fn colour(&mut self, r: Ref, at: (u32, u32)) -> Colour {
        self.colours
            .resolve(r, &mut self.diags, at)
            .unwrap_or(Colour::Direct(ColourValue::rgbt(0.0, 0.0, 0.0, 1.0)))
    }

    // ── Fills ───────────────────────────────────────────────────────────────

    fn paint(&mut self, g: &GradientFill, at: (u32, u32)) -> Paint {
        let c = |m: &mut Mapper<'o>, i: usize| -> Colour {
            let r = g.colours.get(i).copied().unwrap_or(Ref::None);
            m.colour(r, at)
        };
        let from = c(self, 0);
        let to = c(self, 1);
        let mut ramp: Ramp<Colour> = Ramp::new();
        for (pos, cref) in &g.ramp {
            let value = self.colour(*cref, at);
            ramp.insert(RampStop {
                pos: *pos as f32,
                value,
            });
        }
        ramp.profile = BiasGain::new(g.profile.0, g.profile.1);
        let geo = &g.geometry;
        match g.kind {
            FillKind::Linear | FillKind::Linear3Point => {
                if g.kind == FillKind::Linear3Point {
                    self.unsupported(at);
                }
                Paint::Linear {
                    start: geo.start,
                    end: geo.end,
                    persp: None,
                    from,
                    to,
                    ramp,
                }
            }
            FillKind::Circular | FillKind::Elliptical => Paint::Radial {
                centre: geo.start,
                major: geo.end,
                minor: geo.end2.unwrap_or(geo.end),
                aspect_locked: g.kind == FillKind::Circular,
                persp: None,
                from,
                to,
                ramp,
            },
            FillKind::Conical => Paint::Conical {
                centre: geo.start,
                zero_dir: geo.end,
                from,
                to,
                ramp,
            },
            FillKind::Square => Paint::Diamond {
                centre: geo.start,
                corner1: geo.end,
                corner2: geo.end2.unwrap_or(geo.end),
                persp: None,
                from,
                to,
                ramp,
            },
            FillKind::ThreeColour => Paint::ThreeColour {
                origin: geo.start,
                axis1: geo.end,
                axis2: geo.end2.unwrap_or(geo.end),
                c0: from,
                c1: to,
                c2: c(self, 2),
            },
            FillKind::FourColour => {
                let axis2 = geo.end2.unwrap_or(geo.end);
                Paint::FourColour {
                    origin: geo.start,
                    axis1: geo.end,
                    axis2,
                    axis3: opposite_corner(geo.start, geo.end, axis2),
                    c0: from,
                    c1: to,
                    c2: c(self, 2),
                    c3: c(self, 3),
                }
            }
            FillKind::Bitmap | FillKind::ContoneBitmap => {
                let contone = (g.kind == FillKind::ContoneBitmap).then_some((from, to));
                match g.bitmap.and_then(|r| self.bitmap(r, at)) {
                    Some(image) => Paint::Bitmap {
                        image,
                        origin: geo.start,
                        axis_x: geo.end,
                        axis_y: geo.end2.unwrap_or(geo.end),
                        persp: None,
                        tiling: Tiling::default(),
                        dpi: 96,
                        contone,
                        profile: BiasGain::new(g.profile.0, g.profile.1),
                    },
                    None => Paint::Flat {
                        value: Colour::Direct(ColourValue::rgbt(0.0, 0.0, 0.0, 1.0)),
                    },
                }
            }
            FillKind::Fractal => Paint::Fractal {
                params: Box::new(procedural(g.fractal.as_ref())),
                from,
                to,
                profile: BiasGain::new(g.profile.0, g.profile.1),
            },
            FillKind::Noise => Paint::Noise {
                params: Box::new(procedural(g.fractal.as_ref())),
                from,
                to,
                profile: BiasGain::new(g.profile.0, g.profile.1),
            },
        }
    }

    fn transp_paint(&mut self, g: &GradientTransparency, at: (u32, u32)) -> TranspPaint {
        let t = |i: usize| Transparency {
            level: g.levels.get(i).copied().unwrap_or(0),
            mode: g.mode,
        };
        let from = t(0);
        let to = t(1);
        let mut ramp: Ramp<Transparency> = Ramp::new();
        ramp.profile = BiasGain::new(g.profile.0, g.profile.1);
        let geo = &g.geometry;
        match g.kind {
            FillKind::Linear | FillKind::Linear3Point => {
                if g.kind == FillKind::Linear3Point {
                    self.unsupported(at);
                }
                TranspPaint::Linear {
                    start: geo.start,
                    end: geo.end,
                    persp: None,
                    from,
                    to,
                    ramp,
                }
            }
            FillKind::Circular | FillKind::Elliptical => TranspPaint::Radial {
                centre: geo.start,
                major: geo.end,
                minor: geo.end2.unwrap_or(geo.end),
                aspect_locked: g.kind == FillKind::Circular,
                persp: None,
                from,
                to,
                ramp,
            },
            FillKind::Conical => TranspPaint::Conical {
                centre: geo.start,
                zero_dir: geo.end,
                from,
                to,
                ramp,
            },
            FillKind::Square => TranspPaint::Diamond {
                centre: geo.start,
                corner1: geo.end,
                corner2: geo.end2.unwrap_or(geo.end),
                persp: None,
                from,
                to,
                ramp,
            },
            FillKind::ThreeColour => TranspPaint::ThreeColour {
                origin: geo.start,
                axis1: geo.end,
                axis2: geo.end2.unwrap_or(geo.end),
                c0: from,
                c1: to,
                c2: t(2),
            },
            FillKind::FourColour => {
                let axis2 = geo.end2.unwrap_or(geo.end);
                TranspPaint::FourColour {
                    origin: geo.start,
                    axis1: geo.end,
                    axis2,
                    axis3: opposite_corner(geo.start, geo.end, axis2),
                    c0: from,
                    c1: to,
                    c2: t(2),
                    c3: t(3),
                }
            }
            FillKind::Bitmap | FillKind::ContoneBitmap => {
                match g.bitmap.and_then(|r| self.bitmap(r, at)) {
                    Some(image) => TranspPaint::Bitmap {
                        image,
                        origin: geo.start,
                        axis_x: geo.end,
                        axis_y: geo.end2.unwrap_or(geo.end),
                        persp: None,
                        tiling: Tiling::default(),
                        dpi: 96,
                        // The two levels and the mode: black maps to the
                        // start level, white to the end (XARA-T-0307).
                        // Dropping them drew every bitmap transparency
                        // in Mix over the full 0..255 range.
                        contone: Some((from, to)),
                        profile: BiasGain::new(g.profile.0, g.profile.1),
                    },
                    None => TranspPaint::Flat { value: from },
                }
            }
            FillKind::Fractal => TranspPaint::Fractal {
                params: Box::new(procedural(g.fractal.as_ref())),
                from,
                to,
                profile: BiasGain::new(g.profile.0, g.profile.1),
            },
            FillKind::Noise => TranspPaint::Noise {
                params: Box::new(procedural(g.fractal.as_ref())),
                from,
                to,
                profile: BiasGain::new(g.profile.0, g.profile.1),
            },
        }
    }

    fn unsupported(&mut self, at: (u32, u32)) {
        self.diags.push(
            Diagnostic::new(DiagCode::UnknownEnumValue)
                .at(at.0, at.1)
                .with_severity(Severity::Info),
        );
    }
}

// ── Free helpers ────────────────────────────────────────────────────────────

/// How many records a subtree holds, counting its root.
/// Above this many points across its keyholes, a ClipView is clipped by
/// its first keyhole alone rather than by the boolean union of all of
/// them, so that a hostile file cannot make an import crawl.
const MAX_KEYHOLE_POINTS: usize = 200_000;

/// How deep [`silhouettes`] descends into nested groups.
const MAX_KEYHOLE_DEPTH: u32 = 64;

/// A node's own winding rule: a `WindingRule` attribute among its children.
fn own_winding(doc: &xarast_doc::Document, id: xarast_doc::NodeId) -> Option<FillRule> {
    let mut found = None;
    for c in doc.tree.children(id) {
        if let Some(NodeKind::Attr(a)) = doc.tree.kind(c)
            && let AttrValue::WindingRule(r) = &a.value
        {
            found = Some(*r);
        }
    }
    found
}

/// Collects the filled outlines a keyhole contributes to its ClipView's
/// clip, with the winding rule each is filled under. A group contributes
/// its members'. Returns `false` when some part has no outline the
/// importer can compute (text, a live object, an opaque record); the
/// parts that do are still collected.
fn silhouettes(
    doc: &xarast_doc::Document,
    id: xarast_doc::NodeId,
    rule: FillRule,
    out: &mut Vec<(xarast_geom::Path, FillRule)>,
    depth: u32,
) -> bool {
    match doc.tree.kind(id) {
        Some(NodeKind::Path(p)) => {
            out.push(((*p.data).clone(), rule));
            true
        }
        Some(NodeKind::QuickShape(q)) => match &q.path {
            Some(p) => {
                out.push(((**p).clone(), rule));
                true
            }
            None => false,
        },
        Some(NodeKind::Bitmap(b)) => {
            let a = b.origin;
            let at = |p: Point, v: Vector| {
                Point::new(p.x.saturating_add(v.dx), p.y.saturating_add(v.dy))
            };
            let p1 = at(a, b.major);
            let p2 = at(p1, b.minor);
            let p3 = at(a, b.minor);
            let mut pb = xarast_geom::Path::builder();
            pb.move_to(a).line_to(p1).line_to(p2).line_to(p3).close();
            out.push((pb.build(), rule));
            true
        }
        Some(NodeKind::Group(_)) if depth < MAX_KEYHOLE_DEPTH => {
            let mut ok = true;
            for c in doc.tree.children(id) {
                match doc.tree.kind(c) {
                    Some(NodeKind::Attr(_)) | None => {}
                    // A nested ClipView holds clipped content; its keyholes
                    // are its group's other children, visited here.
                    Some(NodeKind::ClipView(_)) => {}
                    Some(_) => {
                        let own = own_winding(doc, c).unwrap_or(rule);
                        ok &= silhouettes(doc, c, own, out, depth.saturating_add(1));
                    }
                }
            }
            ok
        }
        _ => false,
    }
}

fn count_subtree(nodes: &[RecordNode]) -> u32 {
    let mut n = 0u32;
    for node in nodes {
        n = n
            .saturating_add(1)
            .saturating_add(count_subtree(&node.children));
    }
    n
}

/// Millipoints per predefined unit, for the negative unit references of
/// `research/01 §5.2`. `None` for "untyped" and for anything unknown, which
/// leaves the caller's default in place rather than inventing a scale.
fn builtin_unit_mp(v: i32) -> Option<f64> {
    Some(match v {
        -2 => 2_834.652_715,    // millimetre
        -3 => 28_346.527_15,    // centimetre
        -4 => 2_834_652.715,    // metre
        -5 => 2_834_652_715.0,  // kilometre
        -6 => 1.0,              // millipoint
        -7 => 1_000.0,          // point
        -8 => 12_000.0,         // pica
        -9 => 72_000.0,         // inch
        -10 => 864_000.0,       // foot
        -11 => 2_592_000.0,     // yard
        -12 => 4_561_920_000.0, // mile
        -13 => 750.0,           // pixel at 96 dpi
        _ => return None,
    })
}

/// The fourth corner of the parallelogram `origin`, `a`, `b`.
fn opposite_corner(origin: Point, a: Point, b: Point) -> Point {
    Point::new(
        a.x.saturating_add(b.x).saturating_sub(origin.x),
        a.y.saturating_add(b.y).saturating_sub(origin.y),
    )
}

fn procedural(p: Option<&FractalParams>) -> ProceduralParams {
    match p {
        Some(f) => ProceduralParams {
            seed: f.seed,
            graininess: f.graininess as f32,
            gravity: f.gravity as f32,
            squash: f.squash as f32,
            dpi: u32::try_from(f.dpi).unwrap_or(96),
            tileable: f.tileable,
        },
        None => ProceduralParams::default(),
    }
}

/// `TAG_STARTCAP` / `TAG_ENDCAP` hold the original's in-memory cap enum
/// as a byte: 0 butt, 1 round, 2 square (`Kernel/attr.h:126-131`, written
/// and read raw at `Kernel/lineattr.cpp:1863`, `:4174`). The `CXF_CAP_STYLE_*`
/// constants (1..=3) in `Kernel/cxfdefs.h` are never used. The corpus
/// confirms it: only 1 and 2 occur, never the default 0. An out-of-range
/// value strokes round, as the original's stroker does
/// (`wxOil/grndrgn.cpp:1872-1886`).
fn cap(v: u8) -> Cap {
    match v {
        0 => Cap::Butt,
        2 => Cap::Square,
        _ => Cap::Round,
    }
}

/// `TAG_JOINSTYLE` holds the in-memory join enum as a byte: 0 mitre,
/// 1 round, 2 bevelled (`Kernel/attr.h:147-152`, raw at
/// `Kernel/lineattr.cpp:2904`, `:4210`), not the unused
/// `CXF_JOIN_STYLE_*` (1..=3). The corpus has 0 and 1 only: bevelled is
/// the document default and never written. Out of range strokes round
/// (`wxOil/grndrgn.cpp:1887-1902`).
fn join(v: u8) -> Join {
    match v {
        0 => Join::Mitre,
        2 => Join::Bevel,
        _ => Join::Round,
    }
}

/// `TAG_QUALITY` is a 0..110 slider, not an enum.
///
/// The original derives what to draw from thresholds
/// (`Kernel/quality.cpp:157-255`): above 30 lines are real rather than
/// black hairlines, from 60 fills are graduated, and above 100 rendering is
/// antialiased. Those three thresholds are exactly the model's four levels.
fn quality(v: i32) -> xarast_doc::Quality {
    use xarast_doc::Quality as Q;
    match v {
        i32::MIN..=30 => Q::Outline,
        31..=59 => Q::Simple,
        60..=100 => Q::Normal,
        _ => Q::Full,
    }
}

/// The mapping records keep the original's attribute values: a
/// non-repeating record reads back as 1 (`Tiling::Simple`), and the "extra"
/// repeat (206/207) as 4, which is the only value that makes a graduated
/// fill tile. How each fill family interprets them is the walker's business
/// (`docs/research/01-xar-format.md` §8.3).
fn tiling(r: crate::decode::FillRepeat) -> Tiling {
    use crate::decode::FillRepeat as R;
    match r {
        R::None => Tiling::Simple,
        R::Repeat => Tiling::Repeat,
        R::RepeatInverted => Tiling::RepeatInverted,
        R::Extra => Tiling::RepeatExtra,
    }
}

fn image_format(f: crate::decode::BitmapFormat) -> ImageFormat {
    use crate::decode::BitmapFormat as B;
    match f {
        B::Png => ImageFormat::Png,
        B::Jpeg | B::Jpeg8Bpp => ImageFormat::Jpeg,
        B::Gif => ImageFormat::Gif,
        B::Bmp => ImageFormat::Bmp,
        B::BmpZip => ImageFormat::Unknown,
    }
}

/// The record's two paths are edge *templates*, not the outline
/// (`research/01 §4.7.1`). The outline is generated here, in the shape's own
/// untransformed space where the record's parameters are exact, and only then
/// carried through the matrix.
fn quick_shape(s: &RegularShape) -> QuickShape {
    use crate::decode::ShapeFlags as F;
    let m = s.matrix;
    // A two-point template is a straight edge, which is also the default, so
    // only a curved template is worth keeping.
    let template = |p: &xarast_geom::Path| (p.points().len() > 2).then(|| Arc::new(p.clone()));
    let spec = xarast_geom::RegularShapeSpec {
        sides: u32::from(s.sides),
        circular: s.flags.contains(F::CIRCULAR),
        stellated: s.flags.contains(F::STELLATED),
        primary_curved: s.flags.contains(F::PRIMARY_CURVATURE),
        stellation_curved: s.flags.contains(F::STELLATION_CURVATURE),
        centre: Point::ORIGIN,
        major: s.major_axis,
        minor: s.minor_axis,
        stellation_radius: s.stellation_radius,
        stellation_offset: s.stellation_offset,
        primary_curvature: s.primary_curvature,
        stellation_curvature: s.secondary_curvature,
        primary_edge: Some(&s.primary_edge),
        secondary_edge: Some(&s.secondary_edge),
    };
    let path = xarast_geom::regular_shape_outline(&spec).map(|p| Arc::new(p.transformed(m)));
    QuickShape {
        sides: spec.sides,
        circular: spec.circular,
        stellated: spec.stellated,
        curved: spec.primary_curved,
        stellation_curved: spec.stellation_curved,
        centre: m.transform_point(Point::ORIGIN),
        major: m.transform_vector(s.major_axis),
        minor: m.transform_vector(s.minor_axis),
        stellation_radius: s.stellation_radius,
        stellation_offset: s.stellation_offset,
        primary_curvature: s.primary_curvature,
        stellation_curvature: s.secondary_curvature,
        primary_edge: template(&s.primary_edge),
        secondary_edge: template(&s.secondary_edge),
        path,
    }
}

/// `TAG_PATH_FLAGS` is written as the path's **first child**
/// (`research/01 §7.5`), so the path reads it before it is emitted.
fn path_flags_of(children: &[RecordNode]) -> Option<&[u8]> {
    children
        .iter()
        .find(|c| c.record.tag == 111)
        .map(|c| c.record.data.as_slice())
}

fn spread_information_of(children: &[RecordNode]) -> Option<SpreadInformation> {
    let rec = &children.iter().find(|c| c.record.tag == 45)?.record;
    let mut sink = DiagSink::new();
    match decode(
        rec.tag,
        &rec.data,
        Point::ORIGIN,
        &mut sink,
        (rec.number, rec.tag),
    ) {
        Ok(Decoded::SpreadInformation(i)) if i.width.raw() > 0 && i.height.raw() > 0 => Some(i),
        _ => None,
    }
}

fn anim_of(children: &[RecordNode]) -> Option<Box<xarast_doc::AnimProps>> {
    let rec = &children.iter().find(|c| c.record.tag == 4031)?.record;
    let mut sink = DiagSink::new();
    let Ok(Decoded::SpreadAnimProps(w)) = decode(
        rec.tag,
        &rec.data,
        Point::ORIGIN,
        &mut sink,
        (rec.number, rec.tag),
    ) else {
        return None;
    };
    // Seven words: loop, global_delay, dither, web_palette, colours_palette,
    // num_colours, flags (`research/01 §4.3`). `AnimProps` has a home for
    // exactly one of them — the delay — because the other six describe how
    // the spread is *exported* as a GIF rather than what it is. Recorded in
    // `docs/memory/xar-import.md` as a model gap rather than crammed in.
    Some(Box::new(xarast_doc::AnimProps {
        delay: w.get(1).copied().unwrap_or(0),
        hidden: false,
        background: false,
    }))
}

fn layer_from(children: &[RecordNode]) -> LayerNode {
    let mut layer = LayerNode {
        active: false,
        ..LayerNode::default()
    };
    let mut sink = DiagSink::new();
    for c in children {
        let rec = &c.record;
        match rec.tag {
            48 | 49 => {
                if let Ok(Decoded::LayerDetails { flags, name, .. }) = decode(
                    rec.tag,
                    &rec.data,
                    Point::ORIGIN,
                    &mut sink,
                    (rec.number, rec.tag),
                ) {
                    layer.name = Arc::from(name.as_str());
                    layer.visible = flags.contains(LayerFlags::VISIBLE);
                    layer.locked = flags.contains(LayerFlags::LOCKED);
                    layer.printable = flags.contains(LayerFlags::PRINTABLE);
                    layer.active = flags.contains(LayerFlags::ACTIVE);
                    layer.page_background = flags.contains(LayerFlags::PAGE_BACKGROUND);
                    layer.background = flags.contains(LayerFlags::BACKGROUND);
                    layer.guide = rec.tag == 49;
                }
            }
            4030 => {
                if let Ok(Decoded::LayerFrameProps { delay, flags }) = decode(
                    rec.tag,
                    &rec.data,
                    Point::ORIGIN,
                    &mut sink,
                    (rec.number, rec.tag),
                ) {
                    layer.frame = Some(Box::new(xarast_doc::FrameProps {
                        delay,
                        solid: flags & 0x01 != 0,
                        overlay: flags & 0x02 != 0,
                    }));
                }
            }
            _ => {}
        }
    }
    layer
}

/// The attribute values the original starts from where they differ from
/// the model's defaults: text with no size attribute is 16 pt
/// (`Kernel/txtattr.cpp:449-452`), which the model's default now agrees
/// with (XARA-T-0172). The importer mirrors the file's state from these,
/// and writes a value explicitly wherever a node relies on one that the
/// model's defaults do not give.
fn xar_defaults() -> xarast_doc::DefaultAttrs {
    let mut d = xarast_doc::DefaultAttrs::default();
    d.set(AttrValue::FontSize(Mp::new(16_000)));
    d
}

fn text_story(s: &crate::decode::TextStory, tag: u32, children: &[RecordNode]) -> TextStoryNode {
    let transform = match s.placement {
        TextPlacement::Simple(p) => Matrix::translate(Vector::new(p.x, p.y)),
        TextPlacement::Complex(m) => m,
    };
    let mut sink = DiagSink::new();
    let mut column: Option<(Mp, bool)> = None;
    let mut indents = (Mp::ZERO, Mp::ZERO);
    for c in children {
        let rec = &c.record;
        let at = (rec.number, rec.tag);
        match decode(rec.tag, &rec.data, Point::ORIGIN, &mut sink, at) {
            Ok(Decoded::TextWordWrap { width, enabled }) => column = Some((width, enabled)),
            Ok(Decoded::TextIndents { left, right }) => indents = (left, right),
            _ => {}
        }
    }
    let layout = if s.on_path {
        // 2110–2113 and 2114–2117 are `{START,END}_{LEFT,RIGHT}` in that
        // order. `END` means the text runs along the path reversed and
        // `RIGHT` that the characters are reflected (a negative character
        // scale): START_LEFT is plain, END_RIGHT reversed, START_RIGHT
        // reflected, END_LEFT both (`Kernel/cxftext.cpp:671-700`,
        // `Kernel/rechtext.cpp:335-540`). The complex forms add the
        // pre-fit rotation and shear.
        let variant = tag.saturating_sub(2110) % 4;
        TextLayout::OnPath {
            reversed: variant >= 2,
            tangential: true,
            left_indent: indents.0,
            right_indent: indents.1,
            chars: xarast_doc::CharsTransform {
                reflected: variant == 1 || variant == 2,
                rotation: s.rotation.map_or(0, xarast_doc::CharsTransform::fixed),
                shear: s.shear.map_or(0, xarast_doc::CharsTransform::fixed),
            },
        }
    } else {
        match column {
            Some((width, wrap)) if width.raw() > 0 => TextLayout::InColumn {
                width,
                word_wrap: wrap,
            },
            _ => TextLayout::AtPoint,
        }
    };
    TextStoryNode {
        transform,
        layout,
        auto_kern: s.autokern,
        print_as_shapes: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synth::XarBuilder;

    fn info(width: i32, height: i32, margin: i32, bleed: i32, flags: u8) -> SpreadInformation {
        SpreadInformation {
            width: Mp::new(width),
            height: Mp::new(height),
            margin: Mp::new(margin),
            bleed: Mp::new(bleed),
            flags,
        }
    }

    #[test]
    fn the_origin_is_the_pasteboard_margin() {
        let i = info(600_000, 450_000, 576_000, 0, 2);
        assert_eq!(spread_origin(&i), Point::raw(576_000, 576_000));
        assert_eq!(
            pages_rect(&i),
            Rect::new(
                Point::raw(576_000, 576_000),
                Point::raw(1_176_000, 1_026_000)
            )
        );
    }

    #[test]
    fn a_zero_margin_spread_has_a_zero_origin() {
        let i = info(1_533_750, 1_152_000, 0, 0, 0);
        assert_eq!(spread_origin(&i), Point::ORIGIN);
    }

    /// The case no corpus file exercises: `bleed > margin`, where the pages
    /// are pushed out by `margin + bleed` rather than by `margin`.
    #[test]
    fn a_bleed_larger_than_the_margin_widens_the_origin() {
        let i = info(600_000, 450_000, 10_000, 25_000, 0);
        assert_eq!(spread_origin(&i), Point::raw(35_000, 35_000));
        let j = info(600_000, 450_000, 25_000, 10_000, 0);
        assert_eq!(spread_origin(&j), Point::raw(25_000, 25_000));
    }

    #[test]
    fn a_double_page_spread_is_twice_as_wide_from_the_same_corner() {
        let i = info(600_000, 450_000, 72_000, 0, 1);
        assert_eq!(spread_origin(&i), Point::raw(72_000, 72_000));
        assert_eq!(
            pages_rect(&i),
            Rect::new(Point::raw(72_000, 72_000), Point::raw(1_272_000, 522_000))
        );
    }

    #[test]
    fn the_node_kind_names_match_the_model() {
        // `Opaque` is the last discriminant; if the model gains a variant
        // this fails rather than silently mis-labelling a column.
        let opaque = NodeKind::Opaque(Box::new(OpaqueNode {
            tag: 0,
            payload: Arc::from(&[][..]),
        }));
        assert_eq!(usize::from(opaque.discriminant()), NODE_KIND_COUNT - 1);
        assert_eq!(
            NODE_KIND_NAMES.get(usize::from(opaque.discriminant())),
            Some(&opaque.type_name())
        );
    }

    fn minimal_drawing(margin: i32) -> Vec<u8> {
        minimal_drawing_with(margin, 0)
    }

    fn minimal_drawing_with(margin: i32, bleed: i32) -> Vec<u8> {
        let mut spread = Vec::new();
        spread.extend_from_slice(&600_000i32.to_le_bytes());
        spread.extend_from_slice(&450_000i32.to_le_bytes());
        spread.extend_from_slice(&margin.to_le_bytes());
        spread.extend_from_slice(&bleed.to_le_bytes());
        spread.push(2);

        let mut layer = vec![0x01 | 0x04 | 0x08];
        for u in "Layer 1".encode_utf16() {
            layer.extend_from_slice(&u.to_le_bytes());
        }
        layer.extend_from_slice(&0u16.to_le_bytes());

        // A two-point relative path: MoveTo(1000, 2000), LineTo(500, 1500).
        let mut path = vec![0x06u8];
        path.extend_from_slice(&interleave(1000, 2000));
        path.push(0x02);
        path.extend_from_slice(&interleave(500, 500));

        XarBuilder::new()
            .record(40, &[])
            .down()
            .record(41, &[])
            .down()
            .record(42, &[])
            .down()
            .record(45, &spread)
            .record(43, &[])
            .down()
            .record(48, &layer)
            .record(116, &path)
            .down()
            .record(111, &[0x05, 0x05])
            .record(152, &500i32.to_le_bytes())
            .record(194, &[])
            .up()
            .up()
            .up()
            .up()
            .end_of_file()
            .finish()
    }

    fn interleave(x: i32, y: i32) -> [u8; 8] {
        let (x, y) = (x.to_be_bytes(), y.to_be_bytes());
        [x[0], y[0], x[1], y[1], x[2], y[2], x[3], y[3]]
    }

    /// A layer holding one story: a string with its own font size, a string
    /// without, a non-BMP character as two `TAG_TEXT_CHAR` records, an EOL.
    fn text_drawing() -> Vec<u8> {
        let mut spread = Vec::new();
        for v in [600_000i32, 450_000, 0, 0] {
            spread.extend_from_slice(&v.to_le_bytes());
        }
        spread.push(2);
        let mut layer = vec![0x01 | 0x04 | 0x08];
        for u in "L".encode_utf16() {
            layer.extend_from_slice(&u.to_le_bytes());
        }
        layer.extend_from_slice(&0u16.to_le_bytes());
        let mut story = Vec::new();
        for v in [10_000i32, 20_000, 1] {
            story.extend_from_slice(&v.to_le_bytes());
        }
        let utf16 = |s: &str| -> Vec<u8> { s.encode_utf16().flat_map(u16::to_le_bytes).collect() };
        XarBuilder::new()
            .record(40, &[])
            .down()
            .record(41, &[])
            .down()
            .record(42, &[])
            .down()
            .record(45, &spread)
            .record(43, &[])
            .down()
            .record(48, &layer)
            .record(2100, &story)
            .down()
            .record(2200, &[])
            .down()
            .record(2201, &utf16("ab"))
            .down()
            .record(2906, &20_000i32.to_le_bytes())
            .up()
            .record(2201, &utf16("cd"))
            .record(2202, &0xD83Du16.to_le_bytes())
            .record(2202, &0xDE00u16.to_le_bytes())
            .record(2202, &0xDC00u16.to_le_bytes())
            .record(2203, &[])
            .up()
            .up()
            .up()
            .up()
            .up()
            .up()
            .end_of_file()
            .finish()
    }

    #[test]
    fn a_string_s_attributes_apply_to_that_string_only_and_surrogates_pair() {
        let (doc, _) = import(&text_drawing(), &ImportOptions::default()).unwrap();
        assert_eq!(doc.validate().errors, Vec::new());
        let line = doc
            .tree
            .preorder(doc.tree.root())
            .find(|n| matches!(doc.tree.kind(*n), Some(NodeKind::TextLine(_))))
            .unwrap();
        let got: Vec<String> = doc
            .tree
            .children(line)
            .map(|c| match doc.tree.kind(c) {
                Some(NodeKind::TextItem(TextItem::Char(ch))) => ch.to_string(),
                Some(NodeKind::TextItem(TextItem::LineBreak(_))) => "EOL".into(),
                Some(NodeKind::Attr(a)) => match &a.value {
                    AttrValue::FontSize(v) => format!("size {}", v.raw()),
                    other => format!("{other:?}"),
                },
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(
            got,
            [
                "size 20000",
                "a",
                "b",
                "size 16000",
                "c",
                "d",
                "\u{1F600}",
                "\u{FFFD}",
                "EOL"
            ]
        );
        // The story sets no size: it relies on the original's 16 pt, which
        // is the model's default too, so nothing needs to be written.
        let story = doc.tree.links(line).parent.unwrap();
        assert_eq!(
            xarast_doc::default_for(AttrSlot::TxtFontSize),
            AttrValue::FontSize(Mp::new(16_000))
        );
        assert!(!doc.tree.children(story).any(|c| matches!(
            doc.tree.kind(c),
            Some(NodeKind::Attr(a)) if matches!(a.value, AttrValue::FontSize(_))
        )));
    }

    #[test]
    fn a_minimal_drawing_maps_into_a_valid_document() {
        let bytes = minimal_drawing(576_000);
        let (doc, report) = import(&bytes, &ImportOptions::default()).unwrap();
        assert_eq!(doc.validate().errors, Vec::new());
        assert_eq!(report.spread_origin, Point::raw(576_000, 576_000));
        assert_eq!(report.nodes_of(NodeKind::Chapter.discriminant()), 1);
        assert_eq!(
            report.nodes_of(NodeKind::Spread(Box::default()).discriminant()),
            1
        );
        assert_eq!(
            report.nodes_of(NodeKind::Page(Box::default()).discriminant()),
            1
        );
        assert_eq!(
            report.nodes_of(NodeKind::Layer(Box::default()).discriminant()),
            1
        );
        assert_eq!(report.records_opaque, 0);
    }

    #[test]
    fn a_path_lands_in_document_coordinates_with_its_flags() {
        use xarast_geom::PointFlags;
        let bytes = minimal_drawing(576_000);
        let (doc, _) = import(&bytes, &ImportOptions::default()).unwrap();
        let path = doc
            .tree
            .preorder(doc.tree.root())
            .find_map(|id| match doc.tree.kind(id) {
                Some(NodeKind::Path(p)) => Some(p.clone()),
                _ => None,
            })
            .expect("the drawing has one path");
        // Point 0 is absolute and origin-translated; point 1 is an inverted
        // delta from it, and the origin is *not* applied twice.
        assert_eq!(
            path.data.points(),
            &[
                Point::raw(1000 + 576_000, 2000 + 576_000),
                Point::raw(500 + 576_000, 1500 + 576_000)
            ]
        );
        assert_eq!(
            path.data.flags_at(0),
            PointFlags::SMOOTH | PointFlags::END_POINT
        );
        assert!(path.stroked && path.filled);
    }

    #[test]
    fn a_zero_margin_file_puts_the_page_at_the_origin() {
        let bytes = minimal_drawing(0);
        let (doc, report) = import(&bytes, &ImportOptions::default()).unwrap();
        assert_eq!(report.spread_origin, Point::ORIGIN);
        let page = doc
            .tree
            .preorder(doc.tree.root())
            .find_map(|id| match doc.tree.kind(id) {
                Some(NodeKind::Page(p)) => Some(p.rect),
                _ => None,
            })
            .expect("one page");
        assert_eq!(page.lo, Point::ORIGIN);
    }

    /// The case no corpus file exercises, as a whole file rather than as a
    /// call to [`spread_origin`]: every one of the 59 has `bleed == 0`, so
    /// nothing there can tell `margin` from
    /// `margin < bleed ? margin + bleed : margin`. This file can, and it is
    /// synthetic, so nothing of Xara's is copied to get it.
    #[test]
    fn a_synthetic_file_with_a_bleed_wider_than_its_margin_shifts_the_origin() {
        let bytes = minimal_drawing_with(10_000, 25_000);
        let (doc, report) = import(&bytes, &ImportOptions::default()).unwrap();
        assert_eq!(doc.validate().errors, Vec::new());
        // 10 000 < 25 000, so the pages are pushed out by margin + bleed.
        assert_eq!(report.spread_origin, Point::raw(35_000, 35_000));
        let page = doc
            .tree
            .preorder(doc.tree.root())
            .find_map(|id| match doc.tree.kind(id) {
                Some(NodeKind::Page(p)) => Some(p.rect),
                _ => None,
            })
            .expect("one page");
        assert_eq!(page.lo, Point::raw(35_000, 35_000));
        let path = doc
            .tree
            .preorder(doc.tree.root())
            .find_map(|id| match doc.tree.kind(id) {
                Some(NodeKind::Path(p)) => Some(p.clone()),
                _ => None,
            })
            .expect("one path");
        assert_eq!(
            path.data.points().first(),
            Some(&Point::raw(1000 + 35_000, 2000 + 35_000))
        );
    }

    #[test]
    fn caps_and_joins_read_as_the_original_s_enums() {
        assert_eq!(
            [cap(0), cap(1), cap(2), cap(3)],
            [Cap::Butt, Cap::Round, Cap::Square, Cap::Round]
        );
        assert_eq!(
            [join(0), join(1), join(2), join(3)],
            [Join::Mitre, Join::Round, Join::Bevel, Join::Round]
        );
        // An object with no join record strokes bevelled, as in the original.
        assert_eq!(
            xarast_doc::default_for(xarast_doc::AttrSlot::JoinType),
            AttrValue::JoinType(Join::Bevel)
        );
    }

    #[test]
    fn winding_records_read_as_the_original_s_enum_and_none_is_even_odd() {
        // No record, then 0 non-zero, 1 negative, 2 even-odd, 3 positive,
        // and a byte out of range (XARA-T-0331).
        let bytes = layer_with(|b| {
            let mut b = b.record(116, &square_path(0, 0, 10_000));
            for v in [0u8, 1, 2, 3, 9] {
                b = b
                    .record(116, &square_path(0, 0, 10_000))
                    .down()
                    .record(178, &[v])
                    .up();
            }
            b
        });
        let (doc, report) = import(&bytes, &ImportOptions::default()).unwrap();
        balanced(&report);
        let rules: Vec<AttrValue> = doc
            .tree
            .preorder(doc.tree.root())
            .filter(|&n| matches!(doc.tree.kind(n), Some(NodeKind::Path(_))))
            .map(|n| {
                xarast_doc::attr::resolve_uncached(&doc.tree, n, &doc.defaults)
                    .get(xarast_doc::AttrSlot::WindingRule)
                    .clone()
            })
            .collect();
        let want = [
            FillRule::EvenOdd,
            FillRule::NonZero,
            FillRule::Negative,
            FillRule::EvenOdd,
            FillRule::Positive,
            FillRule::NonZero,
        ]
        .map(AttrValue::WindingRule);
        assert_eq!(rules, want);
    }

    #[test]
    fn an_unknown_non_ignorable_record_survives_as_opaque() {
        let bytes = XarBuilder::new()
            .record(40, &[])
            .down()
            .record(107, &[1, 2, 3, 4])
            .up()
            .end_of_file()
            .finish();
        let (doc, report) = import(&bytes, &ImportOptions::default()).unwrap();
        assert_eq!(report.records_opaque, 1);
        let kept = doc
            .tree
            .preorder(doc.tree.root())
            .find_map(|id| match doc.tree.kind(id) {
                Some(NodeKind::Opaque(o)) => Some(o.clone()),
                _ => None,
            })
            .expect("the record was kept");
        assert_eq!(kept.tag, 107);
        assert_eq!(&*kept.payload, &[1, 2, 3, 4]);
    }

    #[test]
    fn current_attributes_are_neither_defaults_nor_nodes() {
        // A current line width and a current black fill: what the editor
        // would give the next object drawn, not what an unattributed object
        // in the file inherits (XARA-T-0037).
        let bytes = XarBuilder::new()
            .record(40, &[])
            .down()
            .record(41, &[])
            .record(4119, &[1])
            .down()
            .record(152, &4_242i32.to_le_bytes())
            .record(191, &[])
            .up()
            .up()
            .end_of_file()
            .finish();
        let (doc, report) = import(&bytes, &ImportOptions::default()).unwrap();
        assert_eq!(report.current_attributes, 2);
        assert_eq!(report.current_differing, 2);
        let factory = xarast_doc::DefaultAttrs::xara_compatible();
        for slot in [
            xarast_doc::AttrSlot::LineWidth,
            xarast_doc::AttrSlot::FillGeometry,
        ] {
            assert_eq!(doc.defaults.get(slot), factory.get(slot), "{slot:?}");
        }
        assert_eq!(
            report.nodes_of(
                NodeKind::Attr(Box::new(xarast_doc::AttrNode::new(AttrValue::LineWidth(
                    Mp::ZERO
                ))))
                .discriminant()
            ),
            0
        );
    }

    #[test]
    fn an_empty_file_is_an_error_rather_than_an_empty_document() {
        let bytes = XarBuilder::new().end_of_file().finish();
        assert!(matches!(
            import(&bytes, &ImportOptions::default()),
            Err(XarError::Build(xarast_doc::BuildError::Empty))
        ));
    }

    #[test]
    fn a_truncated_file_still_produces_a_valid_document() {
        let full = minimal_drawing(576_000);
        for n in 16..full.len() {
            let Some(head) = full.get(..n) else { continue };
            if let Ok((doc, _)) = import(head, &ImportOptions::default()) {
                assert_eq!(doc.validate().errors, Vec::new(), "truncated at {n}");
            }
        }
    }

    /// XARA-T-0013: the record's edge paths are edge templates, not the
    /// outline. Storing one as the shape's path gave every quick shape a
    /// sliver of bounds far from where it sits, so the renderer culled it.
    /// Synthetic parameters shaped like a stellated six-pointed star, placed
    /// by a translation.
    #[test]
    fn a_quick_shape_gets_its_generated_outline_not_its_edge_template() {
        use crate::decode::ShapeFlags;
        let mut edge = xarast_geom::Path::builder();
        edge.move_to(Point::raw(-576_000, -576_000))
            .line_to(Point::raw(-504_000, -576_000));
        let edge = edge.build();
        let at = Point::raw(115_000, 301_000);
        let s = RegularShape {
            flags: ShapeFlags::STELLATED,
            sides: 6,
            major_axis: Vector::new(Mp::new(-27_750), Mp::new(47_250)),
            minor_axis: Vector::new(Mp::new(47_250), Mp::new(27_750)),
            matrix: xarast_geom::Matrix {
                a: 1.0,
                b: 0.0,
                c: 0.0,
                d: 1.0,
                e: at.x,
                f: at.y,
            },
            stellation_radius: 0.5,
            stellation_offset: 0.0,
            primary_curvature: 0.2,
            secondary_curvature: 0.2,
            primary_edge: edge.clone(),
            secondary_edge: edge,
        };
        let q = quick_shape(&s);
        let path = q.path.as_deref().expect("an outline is generated");
        assert_eq!(path.points().len(), 13, "move, then twelve edges");
        let b = path.bounds();
        // The primary radius is |major| = 54 797 mp; nothing is culled.
        assert!(
            b.width().raw() > 90_000 && b.height().raw() > 90_000,
            "{b:?}"
        );
        assert!(
            b.contains(at),
            "the outline sits around the matrix translation"
        );
        for p in path.points() {
            let d = p.distance_to(at);
            assert!(
                (d - 54_797.0).abs() < 3.0 || (d - 27_398.0).abs() < 3.0,
                "every point is a primary or a stellation point: {d}"
            );
        }
        // Straight templates are the default and are not kept.
        assert!(q.primary_edge.is_none() && q.secondary_edge.is_none());
        // Regenerating from the stored parameters gives the same outline.
        assert_eq!(q.outline().as_ref(), Some(path));
    }

    /// Every quick shape is an ellipse when the circular flag is set,
    /// whatever its side count says.
    #[test]
    fn a_circular_quick_shape_is_an_ellipse_spanning_its_axes() {
        use crate::decode::ShapeFlags;
        let s = RegularShape {
            flags: ShapeFlags::CIRCULAR,
            sides: 4,
            major_axis: Vector::new(Mp::new(0), Mp::new(30_000)),
            minor_axis: Vector::new(Mp::new(20_000), Mp::new(0)),
            matrix: xarast_geom::Matrix {
                a: 2.0,
                b: 0.0,
                c: 0.0,
                d: 1.0,
                e: Mp::new(500_000),
                f: Mp::new(0),
            },
            stellation_radius: 0.0,
            stellation_offset: 0.0,
            primary_curvature: 0.0,
            secondary_curvature: 0.0,
            primary_edge: xarast_geom::Path::new(),
            secondary_edge: xarast_geom::Path::new(),
        };
        let q = quick_shape(&s);
        let b = q.path.as_deref().expect("an ellipse").tight_bounds();
        assert!((b.width().raw() - 80_000).abs() <= 4, "{b:?}");
        assert!((b.height().raw() - 60_000).abs() <= 4, "{b:?}");
        assert!(b.contains(Point::raw(500_000, 0)));
    }

    #[test]
    fn the_eight_on_path_tags_say_reversed_and_reflected() {
        // (tag, reversed, reflected): START_LEFT, START_RIGHT, END_LEFT,
        // END_RIGHT, simple then complex.
        let cases = [
            (2110, false, false),
            (2111, false, true),
            (2112, true, true),
            (2113, true, false),
            (2114, false, false),
            (2115, false, true),
            (2116, true, true),
            (2117, true, false),
        ];
        for (tag, rev, refl) in cases {
            let complex = tag >= 2114;
            let s = crate::decode::TextStory {
                placement: if complex {
                    TextPlacement::Complex(Matrix::IDENTITY)
                } else {
                    TextPlacement::Simple(Point::ORIGIN)
                },
                on_path: true,
                autokern: true,
                rotation: complex.then_some(0.5),
                shear: complex.then_some(-0.25),
            };
            let node = text_story(&s, tag, &[]);
            let TextLayout::OnPath {
                reversed, chars, ..
            } = node.layout
            else {
                panic!("{tag}: not on a path");
            };
            assert_eq!((reversed, chars.reflected), (rev, refl), "{tag}");
            let angles = if complex { (32_768, -16_384) } else { (0, 0) };
            assert_eq!((chars.rotation, chars.shear), angles, "{tag}");
        }
    }

    /// A closed `side` square with its bottom-left corner at `(x, y)`.
    fn square_path(x: i32, y: i32, side: i32) -> Vec<u8> {
        let mut p = vec![0x06u8];
        p.extend_from_slice(&interleave(x + side, y + side));
        for (verb, dx, dy) in [
            (0x02, side, 0),
            (0x02, 0, side),
            (0x02, -side, 0),
            (0x03, 0, -side),
        ] {
            p.push(verb);
            p.extend_from_slice(&interleave(dx, dy));
        }
        p
    }

    /// A zero-margin spread and a layer holding whatever `f` adds.
    fn layer_with(f: impl FnOnce(XarBuilder) -> XarBuilder) -> Vec<u8> {
        let mut spread = Vec::new();
        for v in [600_000i32, 450_000, 0, 0] {
            spread.extend_from_slice(&v.to_le_bytes());
        }
        spread.push(2);
        let b = XarBuilder::new()
            .record(40, &[])
            .record(41, &[])
            .down()
            .record(42, &[])
            .down()
            .record(45, &spread)
            .record(43, &[])
            .down();
        f(b).up().up().up().end_of_file().finish()
    }

    fn clip_diags(report: &ImportReport) -> Vec<u64> {
        report
            .diagnostics
            .iter()
            .filter(|d| d.code == DiagCode::ClipViewDegraded)
            .map(|d| d.detail)
            .collect()
    }

    fn balanced(report: &ImportReport) {
        assert_eq!(
            report.records_mapped + report.records_skipped + report.records_stripped,
            report.records_read,
            "every record counted once"
        );
    }

    fn kinds_under(doc: &xarast_doc::Document, id: xarast_doc::NodeId) -> Vec<&'static str> {
        doc.tree
            .children(id)
            .filter_map(|c| doc.tree.kind(c))
            .map(NodeKind::type_name)
            .collect()
    }

    fn first_of(doc: &xarast_doc::Document, name: &str) -> Option<xarast_doc::NodeId> {
        doc.tree
            .preorder(doc.tree.root())
            .find(|&id| doc.tree.kind(id).map(NodeKind::type_name) == Some(name))
    }

    #[test]
    fn a_clip_view_puts_its_keyhole_first_and_keeps_it_painted() {
        // Keyhole, marker, clipped: the keyhole is *before* the marker in
        // the file (`Kernel/ndclpcnt.h:123-133`).
        let bytes = layer_with(|b| {
            b.record(4084, &[])
                .down()
                .record(192, &[])
                .record(116, &square_path(100_000, 100_000, 50_000))
                .record(4085, &[])
                .record(116, &square_path(0, 0, 300_000))
                .up()
        });
        let (doc, report) = import(&bytes, &ImportOptions::default()).unwrap();
        balanced(&report);
        assert_eq!(clip_diags(&report), Vec::<u64>::new());
        let group = first_of(&doc, "Group").expect("the controller is a group");
        assert_eq!(kinds_under(&doc, group), ["Attr", "Path", "ClipView"]);
        let cv = first_of(&doc, "ClipView").unwrap();
        assert_eq!(kinds_under(&doc, cv), ["Path", "Path"]);
        let clip = doc.tree.children(cv).next().unwrap();
        let keyhole = doc
            .tree
            .children(group)
            .find(|&c| matches!(doc.tree.kind(c), Some(NodeKind::Path(_))))
            .unwrap();
        let (Some(NodeKind::Path(a)), Some(NodeKind::Path(b))) =
            (doc.tree.kind(clip), doc.tree.kind(keyhole))
        else {
            unreachable!()
        };
        // One keyhole under the group's own rule: taken verbatim.
        assert_eq!(a.data.points(), b.data.points());
        assert!(a.filled && !a.stroked);
    }

    #[test]
    fn keyholes_under_their_own_winding_rules_clip_to_their_union() {
        // Two overlapping squares, the first even-odd (the default), the
        // second non-zero: the union is one outline, not two, and not the
        // overlap.
        let bytes = layer_with(|b| {
            b.record(4084, &[])
                .down()
                .record(116, &square_path(0, 0, 200_000))
                .record(116, &square_path(100_000, 100_000, 200_000))
                .down()
                .record(178, &[0])
                .up()
                .record(4085, &[])
                .record(116, &square_path(0, 0, 400_000))
                .up()
        });
        let (doc, report) = import(&bytes, &ImportOptions::default()).unwrap();
        balanced(&report);
        let cv = first_of(&doc, "ClipView").unwrap();
        let clip = doc.tree.children(cv).next().unwrap();
        let Some(NodeKind::Path(p)) = doc.tree.kind(clip) else {
            panic!("the clip is a path");
        };
        assert_eq!(p.data.subpaths().count(), 1);
        let area = p.data.signed_area().abs();
        let want = 2.0 * 200_000f64.powi(2) - 100_000f64.powi(2);
        assert!((area - want).abs() < want * 1e-6, "{area} vs {want}");
    }

    #[test]
    fn degenerate_clip_views_still_import_every_record() {
        let bytes = layer_with(|b| {
            b
                // No marker: a group.
                .record(4084, &[])
                .down()
                .record(116, &square_path(0, 0, 10_000))
                .record(116, &square_path(0, 0, 20_000))
                .up()
                // A marker and no keyhole: nothing to clip to.
                .record(4084, &[])
                .down()
                .record(4085, &[])
                .down()
                .record(116, &square_path(0, 0, 30_000))
                .up()
                .record(116, &square_path(0, 0, 40_000))
                .up()
                // A stray marker, with a child.
                .record(4085, &[1, 2, 3])
                .down()
                .record(116, &square_path(0, 0, 50_000))
                .up()
                // An empty controller.
                .record(4084, &[])
                // A text keyhole has no outline here: left out of the clip.
                .record(4084, &[])
                .down()
                .record(2100, &[0u8; 64])
                .record(116, &square_path(0, 0, 60_000))
                .record(4085, &[])
                .record(116, &square_path(0, 0, 70_000))
                .up()
        });
        let (doc, report) = import(&bytes, &ImportOptions::default()).unwrap();
        balanced(&report);
        assert_eq!(report.validation_errors, 0);
        assert_eq!(clip_diags(&report), [0, 3, 1, 2]);
        // Only the last controller clips; every path survives.
        let paths = doc
            .tree
            .preorder(doc.tree.root())
            .filter(|&id| matches!(doc.tree.kind(id), Some(NodeKind::Path(_))))
            .count();
        assert_eq!(paths, 7 + 1, "seven paths and one clip");
        let cv = first_of(&doc, "ClipView").unwrap();
        assert_eq!(kinds_under(&doc, cv), ["Path", "Path"]);
    }

    #[test]
    fn a_bitmap_transparency_keeps_its_levels_and_mode() {
        let mut bmp = Vec::new();
        for u in "b".encode_utf16() {
            bmp.extend_from_slice(&u.to_le_bytes());
        }
        bmp.extend_from_slice(&0u16.to_le_bytes());
        bmp.extend_from_slice(b"\x89PNG\r\n\x1a\n");
        let mut t = Vec::new();
        for v in [0i32, 0, 1000, 0, 0, 1000] {
            t.extend_from_slice(&v.to_le_bytes());
        }
        // Start 115, end 200, Bleach; the bitmap is record 2.
        t.extend_from_slice(&[115, 200, 3]);
        t.extend_from_slice(&2i32.to_le_bytes());
        let bytes = XarBuilder::new()
            .record(68, &bmp)
            .record(40, &[])
            .record(116, &square_path(0, 0, 1000))
            .down()
            .record(171, &t)
            .up()
            .end_of_file()
            .finish();
        let (doc, _) = import(&bytes, &ImportOptions::default()).unwrap();
        let t = doc
            .tree
            .preorder(doc.tree.root())
            .find_map(|id| match doc.tree.kind(id) {
                Some(NodeKind::Attr(a)) => match &a.value {
                    AttrValue::TranspFill(t) => Some(t.clone()),
                    _ => None,
                },
                _ => None,
            })
            .expect("the transparency was imported");
        let TranspPaint::Bitmap { contone, .. } = t else {
            panic!("a bitmap transparency: {t:?}");
        };
        let level = |level| Transparency {
            level,
            mode: xarast_color::TranspMode::Bleach,
        };
        assert_eq!(contone, Some((level(115), level(200))));
    }

    /// `TAG_SHADOWCONTROLLER` for `kind` with the corpus's usual numbers.
    fn shadow_controller(kind: u8) -> Vec<u8> {
        let mut v = vec![kind];
        for n in [5_250i32, 2_220, -3_750, 785_397, 37, 100, 3_000] {
            v.extend_from_slice(&n.to_le_bytes());
        }
        v
    }

    fn shadow_record(bias: f64, gain: f64, darkness: f64) -> Vec<u8> {
        let mut v = Vec::new();
        for f in [bias, gain, darkness] {
            v.extend_from_slice(&f.to_le_bytes());
        }
        v
    }

    fn shadow_diags(report: &ImportReport) -> Vec<u64> {
        report
            .diagnostics
            .iter()
            .filter(|d| d.code == DiagCode::ShadowDegraded)
            .map(|d| d.detail)
            .collect()
    }

    fn only_shadow(doc: &xarast_doc::Document) -> xarast_doc::ShadowParams {
        doc.tree
            .preorder(doc.tree.root())
            .find_map(|id| match doc.tree.kind(id) {
                Some(NodeKind::Live(l)) if l.role == xarast_doc::LiveRole::Controller => {
                    match &l.kind {
                        xarast_doc::LiveKind::Shadow(p) => Some((**p).clone()),
                        _ => None,
                    }
                }
                _ => None,
            })
            .expect("a shadow controller")
    }

    #[test]
    fn a_shadow_controller_becomes_a_live_shadow_with_its_object_as_source() {
        // Controller attributes, the shadow with its colour (built-in -2)
        // and transparency, then the object: the order real files use.
        let bytes = layer_with(|b| {
            b.record(4050, &shadow_controller(1))
                .down()
                .record(152, &500i32.to_le_bytes())
                .record(4051, &shadow_record(0.25, -0.5, 1.0 - 94.0 / 255.0))
                .down()
                .record(166, &[94, 1])
                .record(150, &(-2i32).to_le_bytes())
                .up()
                .record(116, &square_path(100_000, 100_000, 50_000))
                .up()
        });
        let (doc, report) = import(&bytes, &ImportOptions::default()).unwrap();
        balanced(&report);
        assert!(shadow_diags(&report).is_empty());
        assert!(doc.validate().errors.is_empty(), "{:?}", doc.validate());
        let c = first_of(&doc, "Live").unwrap();
        assert_eq!(kinds_under(&doc, c), ["Attr", "Live", "Live"]);
        let parts = xarast_doc::live::parts(&doc.tree, c).expect("a controller");
        assert_eq!(kinds_under(&doc, parts.source), ["Path"]);
        let generated = parts.generated.expect("a generated node");
        assert_eq!(kinds_under(&doc, generated), Vec::<&str>::new());
        let p = only_shadow(&doc);
        assert_eq!(p.kind, xarast_doc::ShadowKind::Wall);
        assert_eq!(p.offset, Point::raw(2_220, -3_750));
        assert_eq!(p.blur, Mp::new(5_250));
        assert_eq!(p.darkness, (1.0 - 94.0 / 255.0) as f32);
        assert_eq!(p.opacity_level(), 161);
        assert_eq!(p.profile, BiasGain::new(0.25, -0.5));
        assert_eq!(p.scale, 0.37);
        assert!((p.tilt - 0.785_397).abs() < 1e-6);
        assert_eq!(p.glow_width, Mp::new(3_000));
        let built_in = crate::colour::ColourRegistry::new()
            .resolve(Ref::parse(-2), &mut DiagSink::new(), (1, 150))
            .unwrap();
        assert_eq!(p.colour, built_in);
        // The shadow's transparency and colour are folded into the
        // parameters, not left as attributes anywhere.
        let fills = doc
            .tree
            .preorder(doc.tree.root())
            .filter(|&id| {
                matches!(doc.tree.kind(id), Some(NodeKind::Attr(a))
                    if matches!(a.value, AttrValue::Fill(_) | AttrValue::TranspFill(_)))
            })
            .count();
        assert_eq!(fills, 0);
        // The controller's bounds cover the moved, blurred shadow.
        let mut doc = doc;
        doc.update_bounds();
        let b = doc.tree.bounds(c).get().unwrap();
        assert!(b.hi.x.raw() >= 150_000 + 2_220 + 5_250, "{b:?}");
        assert!(b.lo.y.raw() <= 100_000 - 3_750 - 5_250, "{b:?}");
    }

    #[test]
    fn shadow_types_and_degraded_shadows_are_reported() {
        let bytes = layer_with(|b| {
            b.record(4050, &shadow_controller(2))
                .down()
                .record(4051, &shadow_record(0.0, 0.0, 0.5)[..16])
                .record(116, &square_path(0, 0, 50_000))
                .up()
                .record(4050, &shadow_controller(3))
                .down()
                .record(116, &square_path(0, 0, 50_000))
                .up()
                .record(4050, &shadow_controller(4))
                .down()
                .record(4051, &shadow_record(0.0, 0.0, 0.5))
                .down()
                .record(153, &[0u8; 40])
                .up()
                .up()
                // A stray shadow: its colour must not reach the path after it.
                .record(4051, &shadow_record(0.0, 0.0, 0.5))
                .down()
                .record(150, &(-3i32).to_le_bytes())
                .up()
                .record(116, &square_path(0, 0, 50_000))
        });
        let (doc, report) = import(&bytes, &ImportOptions::default()).unwrap();
        balanced(&report);
        assert!(doc.validate().errors.is_empty(), "{:?}", doc.validate());
        // 0: the glow has no TAG_SHADOW; 3: type 4; 2: a gradient fill;
        // 1: the stray one.
        assert_eq!(shadow_diags(&report), [0, 3, 2, 1]);
        let kinds: Vec<(xarast_doc::ShadowKind, f32)> = doc
            .tree
            .preorder(doc.tree.root())
            .filter_map(|id| match doc.tree.kind(id) {
                Some(NodeKind::Live(l)) if l.role == xarast_doc::LiveRole::Controller => {
                    match &l.kind {
                        xarast_doc::LiveKind::Shadow(p) => Some((p.kind, p.darkness)),
                        _ => None,
                    }
                }
                _ => None,
            })
            .collect();
        // An old 16-byte shadow is fully dark; no shadow at all keeps the
        // default.
        assert_eq!(
            kinds,
            [
                (xarast_doc::ShadowKind::Floor, 1.0),
                (xarast_doc::ShadowKind::Glow, 1.0),
                (xarast_doc::ShadowKind::Wall, 0.5),
            ]
        );
        let stray_fill = doc.tree.preorder(doc.tree.root()).any(|id| {
            matches!(doc.tree.kind(id), Some(NodeKind::Attr(a)) if matches!(a.value, AttrValue::Fill(_)))
        });
        assert!(!stray_fill);
    }

    /// `TAG_BEVEL` with the corpus's usual layout.
    fn bevel_record(style: i32, outer: i32) -> Vec<u8> {
        let mut v = Vec::new();
        for n in [style, 3_951, 313, outer, 47, 45] {
            v.extend_from_slice(&n.to_le_bytes());
        }
        v
    }

    fn bevel_diags(report: &ImportReport) -> Vec<u64> {
        report
            .diagnostics
            .iter()
            .filter(|d| d.code == DiagCode::BevelDegraded)
            .map(|d| d.detail)
            .collect()
    }

    fn bevels(doc: &xarast_doc::Document) -> Vec<xarast_doc::BevelParams> {
        doc.tree
            .preorder(doc.tree.root())
            .filter_map(|id| match doc.tree.kind(id) {
                Some(NodeKind::Live(l)) if l.role == xarast_doc::LiveRole::Controller => {
                    match &l.kind {
                        xarast_doc::LiveKind::Bevel(p) => Some((**p).clone()),
                        _ => None,
                    }
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn an_inner_bevel_controller_becomes_a_live_bevel_with_its_object_as_source() {
        // Controller attributes, the object, then the bevel node with its
        // copies of the object's attributes: the order inner bevels use.
        let bytes = layer_with(|b| {
            b.record(4052, &bevel_record(6, 0))
                .down()
                .record(152, &500i32.to_le_bytes())
                .record(116, &square_path(100_000, 100_000, 50_000))
                .record(4057, &[])
                .down()
                .record(166, &[0, 1])
                .record(163, &[])
                .up()
                .up()
        });
        let (doc, report) = import(&bytes, &ImportOptions::default()).unwrap();
        balanced(&report);
        assert!(bevel_diags(&report).is_empty());
        assert!(doc.validate().errors.is_empty(), "{:?}", doc.validate());
        let c = first_of(&doc, "Live").unwrap();
        assert_eq!(kinds_under(&doc, c), ["Attr", "Live", "Live"]);
        let parts = xarast_doc::live::parts(&doc.tree, c).expect("a controller");
        assert_eq!(kinds_under(&doc, parts.source), ["Path"]);
        let generated = parts.generated.expect("a generated node");
        assert_eq!(kinds_under(&doc, generated), Vec::<&str>::new());
        let [p] = bevels(&doc).try_into().unwrap();
        assert_eq!(
            p,
            xarast_doc::BevelParams {
                bevel_type: xarast_doc::BevelType::Smooth1,
                indent: Mp::new(3_951),
                outer: false,
                light_angle: 313.0,
                light_tilt: 45.0,
                contrast: 0.47,
                colour: None,
            }
        );
        // The bevel node's attributes repeat the object's: none survives.
        let transparencies = doc
            .tree
            .preorder(doc.tree.root())
            .filter(|&id| {
                matches!(doc.tree.kind(id), Some(NodeKind::Attr(a))
                    if matches!(a.value, AttrValue::TranspFill(_)))
            })
            .count();
        assert_eq!(transparencies, 0);
        // An inner bevel stays inside its object.
        let mut doc = doc;
        doc.update_bounds();
        let b = doc.tree.bounds(c).get().unwrap();
        assert_eq!(b, doc.tree.bounds(parts.source).get().unwrap());
    }

    #[test]
    fn an_outer_bevel_takes_its_colour_and_degraded_bevels_are_reported() {
        let bytes = layer_with(|b| {
            // Outer, the bevel node first with its own flat fill.
            b.record(4052, &bevel_record(8, 1))
                .down()
                .record(4057, &[])
                .down()
                .record(150, &(-2i32).to_le_bytes())
                .up()
                .record(116, &square_path(0, 0, 50_000))
                .up()
                // No bevel node, an old 20-byte record (tilt 45).
                .record(4052, &bevel_record(0, 0)[..20])
                .down()
                .record(116, &square_path(0, 0, 50_000))
                .up()
                // A style out of range, and a gradient on the bevel node.
                .record(4052, &bevel_record(99, 1))
                .down()
                .record(116, &square_path(0, 0, 50_000))
                .record(4057, &[])
                .down()
                .record(153, &[0u8; 40])
                .up()
                .up()
                // A stray bevel node: its fill must not reach the path
                // after it. A discarded bevel attribute.
                .record(4057, &[])
                .down()
                .record(150, &(-3i32).to_le_bytes())
                .up()
                .record(4053, &750i32.to_le_bytes())
                .record(116, &square_path(0, 0, 50_000))
        });
        let (doc, report) = import(&bytes, &ImportOptions::default()).unwrap();
        balanced(&report);
        assert!(doc.validate().errors.is_empty(), "{:?}", doc.validate());
        // 0: no bevel node; 3: style 99; 2: a gradient; 1: the stray one.
        assert_eq!(bevel_diags(&report), [0, 3, 2, 1]);
        let got = bevels(&doc);
        assert_eq!(got.len(), 3);
        let built_in = crate::colour::ColourRegistry::new()
            .resolve(Ref::parse(-2), &mut DiagSink::new(), (1, 150))
            .unwrap();
        assert_eq!(got[0].bevel_type, xarast_doc::BevelType::Point1);
        assert!(got[0].outer);
        assert_eq!(got[0].colour, Some(built_in));
        assert_eq!(got[1].light_tilt, 45.0);
        assert_eq!(got[1].colour, None);
        assert_eq!(got[2].bevel_type, xarast_doc::BevelType::Flat);
        assert!(got[2].colour.is_some());
        let stray_fill = doc.tree.preorder(doc.tree.root()).any(|id| {
            matches!(doc.tree.kind(id), Some(NodeKind::Attr(a)) if matches!(a.value, AttrValue::Fill(_)))
        });
        assert!(!stray_fill);
        // An outer bevel's controller covers its faces.
        let mut doc = doc;
        doc.update_bounds();
        let c = first_of(&doc, "Live").unwrap();
        let b = doc.tree.bounds(c).get().unwrap();
        let parts = xarast_doc::live::parts(&doc.tree, c).expect("a controller");
        let src = doc.tree.bounds(parts.source).get().unwrap();
        assert_eq!(b.hi.x.raw(), src.hi.x.raw() + 3_951);
    }

    /// `TAG_CONTOURCONTROLLER`: steps, width (negative: outer), type,
    /// object and attribute profiles.
    fn contour_record(steps: i32, width: i32, kind: u8) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&steps.to_le_bytes());
        v.extend_from_slice(&width.to_le_bytes());
        v.push(kind);
        for f in [0.25f64, -0.5, 0.125, 0.0] {
            v.extend_from_slice(&f.to_le_bytes());
        }
        v
    }

    fn contours(doc: &xarast_doc::Document) -> Vec<xarast_doc::ContourParams> {
        doc.tree
            .preorder(doc.tree.root())
            .filter_map(|id| match doc.tree.kind(id) {
                Some(NodeKind::Live(l)) if l.role == xarast_doc::LiveRole::Controller => {
                    match &l.kind {
                        xarast_doc::LiveKind::Contour(p) => Some((**p).clone()),
                        _ => None,
                    }
                }
                _ => None,
            })
            .collect()
    }

    fn contour_diags(report: &ImportReport) -> Vec<u64> {
        report
            .diagnostics
            .iter()
            .filter(|d| d.code == DiagCode::ContourDegraded)
            .map(|d| d.detail)
            .collect()
    }

    #[test]
    fn an_outer_contour_keeps_its_node_first_with_its_own_attributes() {
        // The layout testimp1 uses: controller attributes, the contour node
        // with its fill, then the object.
        let bytes = layer_with(|b| {
            b.record(4066, &contour_record(3, -16_714, 0))
                .down()
                .record(176, &[0])
                .record(152, &500i32.to_le_bytes())
                .record(4067, &[])
                .down()
                .record(150, &(-2i32).to_le_bytes())
                .up()
                .record(116, &square_path(100_000, 100_000, 50_000))
                .up()
        });
        let (doc, report) = import(&bytes, &ImportOptions::default()).unwrap();
        balanced(&report);
        assert!(contour_diags(&report).is_empty());
        assert_eq!(report.records_stripped, 0);
        assert!(doc.validate().errors.is_empty(), "{:?}", doc.validate());
        let c = first_of(&doc, "Live").unwrap();
        assert_eq!(kinds_under(&doc, c), ["Attr", "Attr", "Live", "Live"]);
        let parts = xarast_doc::live::parts(&doc.tree, c).expect("a controller");
        let generated = parts.generated.expect("a generated node");
        assert_eq!(kinds_under(&doc, generated), ["Attr"]);
        assert_eq!(kinds_under(&doc, parts.source), ["Path"]);
        // The generated node comes first: its rings are beneath the object.
        assert_eq!(doc.tree.children(c).nth(2), Some(generated));
        let [p] = contours(&doc).try_into().unwrap();
        assert_eq!(
            p,
            xarast_doc::ContourParams {
                steps: 3,
                width: Mp::new(16_714),
                outer: true,
                include_line_widths: false,
                join: None,
                profile: BiasGain::new(0.25, -0.5),
                colour_profile: BiasGain::new(0.125, 0.0),
                colour_effect: xarast_color::FillEffect::Fade,
                inset: false,
            }
        );
        // The controller covers its rings.
        let mut doc = doc;
        doc.update_bounds();
        let b = doc.tree.bounds(c).get().unwrap();
        let src = doc.tree.bounds(parts.source).get().unwrap();
        assert!(b.hi.x.raw() >= src.hi.x.raw() + 16_714);
    }

    #[test]
    fn an_inner_contour_follows_its_object_and_degraded_ones_are_reported() {
        let bytes = layer_with(|b| {
            // Inner, rainbow, the node after the object.
            b.record(4066, &contour_record(6, 1_088, 1))
                .down()
                .record(116, &square_path(0, 0, 50_000))
                .record(4067, &[])
                .down()
                .record(150, &(-2i32).to_le_bytes())
                .up()
                .up()
                // No contour node; an inset path; a colour type out of range.
                .record(4066, &contour_record(2, 500, 0x85))
                .down()
                .record(116, &square_path(0, 0, 50_000))
                .up()
                // Too many steps, and a record that stops before its profiles.
                .record(4066, &contour_record(100_000, -700, 2)[..9])
                .down()
                .record(4067, &[])
                .record(116, &square_path(0, 0, 50_000))
                .up()
                // A stray contour node: its fill must not reach the path
                // after it.
                .record(4067, &[])
                .down()
                .record(150, &(-3i32).to_le_bytes())
                .up()
                .record(116, &square_path(0, 0, 50_000))
        });
        let (doc, report) = import(&bytes, &ImportOptions::default()).unwrap();
        balanced(&report);
        assert!(doc.validate().errors.is_empty(), "{:?}", doc.validate());
        // 2: type 5; 0: no node; 3: steps; 1: the stray node.
        assert_eq!(contour_diags(&report), [2, 0, 3, 1]);
        let c = first_of(&doc, "Live").unwrap();
        let parts = xarast_doc::live::parts(&doc.tree, c).expect("a controller");
        let generated = parts.generated.unwrap();
        assert_eq!(doc.tree.children(c).next_back(), Some(generated));
        let got = contours(&doc);
        assert_eq!(got.len(), 3);
        assert!(!got[0].outer);
        assert_eq!(got[0].width, Mp::new(1_088));
        assert_eq!(got[0].colour_effect, xarast_color::FillEffect::Rainbow);
        assert!(got[1].inset);
        assert_eq!(got[1].rings(), 1);
        assert_eq!(got[2].rings(), xarast_doc::live::MAX_CONTOUR_RINGS);
        assert_eq!(got[2].profile, BiasGain::IDENTITY);
        assert_eq!(got[2].colour_effect, xarast_color::FillEffect::AltRainbow);
        let stray_fill = doc.tree.preorder(doc.tree.root()).any(|id| {
            matches!(doc.tree.kind(id), Some(NodeKind::Attr(a))
                if a.value == AttrValue::Fill(Paint::Flat { value: crate::colour::ColourRegistry::new()
                    .resolve(Ref::parse(-3), &mut DiagSink::new(), (1, 150)).unwrap() }))
        });
        assert!(!stray_fill);
    }

    fn blend_diags(report: &ImportReport) -> Vec<u64> {
        report
            .diagnostics
            .iter()
            .filter(|d| d.code == DiagCode::BlendDegraded)
            .map(|d| d.detail)
            .collect()
    }

    fn blends(doc: &xarast_doc::Document) -> Vec<(xarast_doc::NodeId, xarast_doc::BlendParams)> {
        doc.tree
            .preorder(doc.tree.root())
            .filter_map(|id| match doc.tree.kind(id) {
                Some(NodeKind::Live(l)) if l.role == xarast_doc::LiveRole::Controller => {
                    match &l.kind {
                        xarast_doc::LiveKind::Blend(p) => Some((id, (**p).clone())),
                        _ => None,
                    }
                }
                _ => None,
            })
            .collect()
    }

    fn doubles(xs: &[f64]) -> Vec<u8> {
        xs.iter().flat_map(|x| x.to_le_bytes()).collect()
    }

    fn blender_extra(s: i32, e: i32) -> Vec<u8> {
        [0i32, -1, s, e]
            .iter()
            .flat_map(|x| x.to_le_bytes())
            .chain([0u8])
            .collect()
    }

    #[test]
    fn a_blend_keeps_its_objects_with_a_generated_node_between_each_pair() {
        // The layout every corpus blend has: its profiles before it, its
        // attributes, then object, blender (+ additional), object …
        let bytes = layer_with(|b| {
            b.record(4072, &doubles(&[0.25, -0.5, 0.125, 0.0, 0.0, 0.0]))
                .record(105, &[7, 0, 0x13])
                .down()
                .record(176, &[0])
                .record(116, &square_path(0, 0, 10_000))
                .record(106, &[45, 0, 0, 0, 0xff, 0xff, 0xff, 0xff])
                .record(4073, &blender_extra(0, 2))
                .record(116, &square_path(50_000, 0, 20_000))
                .record(106, &[0, 0, 0, 0, 3, 0, 0, 0])
                .record(4073, &blender_extra(2, 4))
                .record(116, &square_path(100_000, 0, 5_000))
                .up()
        });
        let (doc, report) = import(&bytes, &ImportOptions::default()).unwrap();
        balanced(&report);
        assert!(blend_diags(&report).is_empty(), "{:?}", report.diagnostics);
        assert_eq!(report.records_stripped, 0);
        assert!(doc.validate().errors.is_empty(), "{:?}", doc.validate());
        let [(c, p)] = blends(&doc).try_into().unwrap();
        assert_eq!(kinds_under(&doc, c), ["Attr", "Live"]);
        let source = xarast_doc::live::parts(&doc.tree, c).unwrap().source;
        assert_eq!(
            kinds_under(&doc, source),
            ["Path", "Live", "Path", "Live", "Path"]
        );
        assert_eq!(p.steps, 7);
        assert!(p.one_to_one && p.antialias && !p.tangential);
        assert_eq!(p.colour_effect, Some(xarast_color::FillEffect::Rainbow));
        assert_eq!(p.profile, BiasGain::new(0.25, -0.5));
        assert_eq!(p.attr_profile, BiasGain::new(0.125, 0.0));
        assert_eq!(p.blenders.len(), 2);
        assert_eq!(
            (p.blenders[0].start_index, p.blenders[0].end_index),
            (Some(45), None)
        );
        assert_eq!(
            (p.blenders[1].start_index, p.blenders[1].end_index),
            (Some(0), Some(3))
        );
        // Every pair draws its steps.
        let groups = xarast_doc::blend::generate(&doc, c).unwrap();
        assert_eq!(groups.iter().map(Vec::len).collect::<Vec<_>>(), [7, 7]);
    }

    #[test]
    fn degraded_blends_are_reported_and_still_import() {
        let bytes = layer_with(|b| {
            // A colour type out of range, too many steps, no profiles.
            b.record(105, &[0xff, 0xff, 0x52])
                .down()
                .record(116, &square_path(0, 0, 10_000))
                .record(106, &[0; 8])
                .record(4073, &blender_extra(5, 9))
                .record(116, &square_path(0, 0, 10_000))
                .up()
                // Along a path, and a blender with nothing after it.
                .record(105, &[2, 0, 2])
                .down()
                .record(116, &square_path(0, 0, 10_000))
                .record(106, &[0; 8])
                .down()
                .record(4060, &doubles(&[0.0, 1.0]))
                .up()
                .record(116, &square_path(0, 0, 10_000))
                .record(4061, &square_path(0, 0, 10_000))
                .record(106, &[0; 8])
                .up()
                // A stray blender, and a short blend record.
                .record(106, &[0; 8])
                .record(105, &[2, 0])
        });
        let (doc, report) = import(&bytes, &ImportOptions::default()).unwrap();
        balanced(&report);
        assert!(doc.validate().errors.is_empty(), "{:?}", doc.validate());
        // 2 colour, 5 steps, 4 indices; 3 along a path, 0 no end object;
        // 1 the stray blender.
        assert_eq!(blend_diags(&report), [2, 5, 4, 3, 0, 1]);
        let got = blends(&doc);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].1.steps, 65_535);
        assert_eq!(got[0].1.drawn_steps(), 999);
        assert_eq!(got[0].1.colour_effect, Some(xarast_color::FillEffect::Fade));
        assert_eq!(got[1].1.blenders.len(), 2);
        // The path the second blend ran along is not one of its objects.
        let source = xarast_doc::live::parts(&doc.tree, got[1].0).unwrap().source;
        assert_eq!(kinds_under(&doc, source), ["Path", "Live", "Path", "Live"]);
    }
}
