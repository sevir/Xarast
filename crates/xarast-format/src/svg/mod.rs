//! The Xarast SVG profile, writing side (`research/06 §5`, `§6`; phase 6
//! workstream W3).
//!
//! [`write_svg`] turns a [`Document`] into the text of `document.svg`: plain
//! SVG 1.1 that a browser or Inkscape renders reasonably (the *base*
//! representation), plus the `xarast:` attributes and elements that carry
//! what SVG cannot say (the *parametric* representation), and nothing more
//! (rule 5 of §5.1).
//!
//! # What is where
//!
//! | Module | Pass / section |
//! |---|---|
//! | [`num`] | §4.5.1 pass 1: numbers in points, 3 decimals, no redundant zeros |
//! | [`pathdata`] | pass 2: relative/absolute per segment, collapsed commands, `h`/`v`/`s` |
//! | `paint` | §6.3–§6.6: fills, gradients, ramp baking, transparency, masks |
//! | `effect` | §6.8: live effects drawn as SVG filters (feathers) |
//! | `emit` | §5.8, §6.1, §6.2, §6.7–§6.10: the tree, geometry, text, live effects |
//! | `defs` | passes 6–7: content-hashed ids, deduplicated `<defs>` |
//! | [`frame`] | §5.5: Y-up document space → Y-down SVG space, in integers |
//! | [`xml`] | escaping and the well-formedness guarantees |
//! | `style` | passes 4–5: paint hoisted onto `<g>`, CSS paint classes |
//! | `text` | §6.7: text runs, their twins, and the [`TextPlacer`] hook |
//! | [`interchange`] | [`SvgDialect::Interchange`]: the base layer alone, for a standalone `.svg` (phase 11 W11.3) |
//!
//! Passes 3 (default elision) and 8 (minimal indentation) are done inline.
//! Passes 4 (attribute hoisting) and 5 (CSS classes) work on paint *slots*
//! that the walk leaves in the start tags and expands at the end (`style`).
//!
//! # Security (F3.11)
//!
//! The writer never produces `<script>`, event attributes,
//! `<foreignObject>`, SMIL, a DOCTYPE or an entity. Every `href` it writes
//! is either `#id` or a `resources/…` path inside the package — or, in the
//! interchange dialect, whatever [`SvgOptions::bitmaps`] returns (the
//! exporter's `data:` URIs and sidecar paths). The only
//! text it does not generate itself is preserved foreign baggage, which is
//! re-emitted verbatim only when it is a well-formed fragment; stripping
//! active content from baggage is the reader's job (`§5.3`).

mod bake;
pub mod defs;
mod effect;
mod emit;
pub mod frame;
pub mod interchange;
pub mod num;
mod paint;
pub mod pathdata;
pub mod photo;
pub mod read;
mod style;
mod text;
pub mod xml;

pub use read::{ReadOptions, SvgRead, SvgReadError, normal_form, read_svg};
pub use text::{FontFile, PlacedFace, Placer, StoryPlacement, TextPlacer};

use std::collections::HashMap;
use std::sync::Arc;

use xarast_doc::resources::ImageFormat;
use xarast_doc::{BitmapId, Document, NodeKind};

use crate::resource::{ResourceId, ResourceIndex, ResourceKind, resource_path};

/// The SVG namespace.
pub const NS_SVG: &str = "http://www.w3.org/2000/svg";
/// The `xlink` namespace.
pub const NS_XLINK: &str = "http://www.w3.org/1999/xlink";
/// The `xarast` namespace (`research/06 §5.2`); the major version is in it.
pub const NS_XARAST: &str = "https://xarast.org/ns/document/1.0";
/// Inkscape's namespace.
pub const NS_INKSCAPE: &str = "http://www.inkscape.org/namespaces/inkscape";
/// Sodipodi's namespace, which Inkscape still uses for its named view.
pub const NS_SODIPODI: &str = "http://sodipodi.sourceforge.net/DTD/sodipodi-0.dtd";
/// Dublin Core.
pub const NS_DC: &str = "http://purl.org/dc/elements/1.1/";
/// Creative Commons.
pub const NS_CC: &str = "http://creativecommons.org/ns#";
/// RDF.
pub const NS_RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
/// The `xml:` namespace.
pub const NS_XML: &str = "http://www.w3.org/XML/1998/namespace";

/// What a verbatim fragment is, for the well-formedness check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FragmentKind {
    /// Exactly one element.
    Element,
    /// `<!-- … -->`.
    Comment,
    /// `<? … ?>`.
    ProcessingInstruction,
}

/// The one parameter that separates the `.xarast` writer from the
/// interchange exporter (phase 11 W11.3, T11.3.1). The mapper is the same
/// code path for both; see `docs/memory/xarast-format.md` for the exact
/// list of what differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SvgDialect {
    /// Inside `.xarast`: the `xarast:` parametric layer, stable ids,
    /// preserved foreign data, `resources/…` hrefs.
    #[default]
    Native,
    /// A standalone `.svg` for browsers and Inkscape: the base layer only.
    /// No `xarast:` vocabulary (the [`interchange`] projection), no foreign
    /// baggage (counted in [`Stats::foreign_omitted`]), no package
    /// resources (bitmaps go through [`SvgOptions::bitmaps`]).
    Interchange,
}

/// Where an interchange SVG's bitmaps go: called once per bitmap the
/// document draws, it returns the `href` to write (a `data:` URI or a
/// relative path), or `None` when the bitmap cannot be written. Compared
/// by identity.
#[derive(Clone)]
#[allow(clippy::type_complexity)]
pub struct BitmapLinker(
    pub  std::sync::Arc<
        dyn Fn(BitmapId, &xarast_doc::BitmapResource) -> Option<String> + Send + Sync,
    >,
);

impl std::fmt::Debug for BitmapLinker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BitmapLinker(..)")
    }
}

impl PartialEq for BitmapLinker {
    fn eq(&self, other: &BitmapLinker) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for BitmapLinker {}

/// Where an interchange SVG's photo-adjusted bitmaps go (W10.6): called
/// once per bitmap object with photo operations, with the master's id
/// and resource and the object's chain, it returns the `href` of the
/// adjusted image, or `None` to show the master (counted in
/// [`Stats::photo_ops_unbaked`]). Compared by identity.
#[derive(Clone)]
#[allow(clippy::type_complexity)]
pub struct DerivedLinker(
    pub  std::sync::Arc<
        dyn Fn(BitmapId, &xarast_doc::BitmapResource, &xarast_doc::PhotoOps) -> Option<String>
            + Send
            + Sync,
    >,
);

impl std::fmt::Debug for DerivedLinker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DerivedLinker(..)")
    }
}

impl PartialEq for DerivedLinker {
    fn eq(&self, other: &DerivedLinker) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for DerivedLinker {}

/// Options for [`write_svg`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SvgOptions {
    /// Indent one space per level (pass 8's `--pretty`), for diffs.
    pub pretty: bool,
    /// Pass 4: move paint every child of a `<g>` shares onto the `<g>`.
    /// On by default; off only to test the passes apart.
    pub hoist: bool,
    /// Pass 5: `class="cN"` for paint sets shared by ≥ 8 elements. On by
    /// default; off only to test the passes apart.
    pub classes: bool,
    /// Where the application lays text out (`research/06 §6.7`): with it,
    /// the base SVG places every character where Xarast draws it; without
    /// it, lines start at the story's origin. The `xarast:` twin, and so
    /// what a reader rebuilds, is the same either way.
    pub text: Option<Placer>,
    /// Native (inside `.xarast`, the default) or Interchange.
    pub dialect: SvgDialect,
    /// The document rectangle (millipoints, y up) the root `viewBox`,
    /// `width` and `height` frame. `None`: the first spread's pages, as
    /// `.xarast` always does. Coordinates do not change either way.
    pub area: Option<xarast_geom::Rect>,
    /// Interchange only: a rectangle of this colour under everything,
    /// covering the root `viewBox` (an opaque export background). `None`:
    /// transparent.
    pub background: Option<xarast_color::Rgba8>,
    /// Interchange only: drop ids nothing refers to, comments and the
    /// indentation between elements.
    pub minify: bool,
    /// Interchange only: where bitmaps go. `None` writes them into the
    /// resource index as `.xarast` does.
    pub bitmaps: Option<BitmapLinker>,
    /// Interchange only: bakes the photo operations of a bitmap object
    /// into an image of its own. `None` shows the master.
    pub derived_bitmaps: Option<DerivedLinker>,
}

impl Default for SvgOptions {
    fn default() -> SvgOptions {
        SvgOptions {
            pretty: false,
            hoist: true,
            classes: true,
            text: None,
            dialect: SvgDialect::Native,
            area: None,
            background: None,
            minify: false,
            bitmaps: None,
            derived_bitmaps: None,
        }
    }
}

/// What a write produced and what it had to approximate: the numbers
/// the conformance report and the corpus sweep are built from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[allow(missing_docs)]
pub struct Stats {
    /// Model nodes written as elements.
    pub elements: usize,
    pub spreads: usize,
    pub layers: usize,
    pub groups: usize,
    pub paths: usize,
    pub rects: usize,
    pub ellipses: usize,
    pub circles: usize,
    pub quickshapes: usize,
    /// Quick shapes whose outline could not be generated: written with no
    /// `d`, so they draw nothing.
    pub quickshapes_without_outline: usize,
    pub images: usize,
    /// Distinct bitmaps written into the package, what `meta.xml` counts:
    /// a bitmap no element references is not written (XARA-T-0110).
    pub bitmaps: usize,
    /// Bitmaps that could not be written at all (no bytes).
    pub images_missing: usize,
    /// Bitmaps written in a container browsers cannot decode (BMP, the
    /// compressed BMP of `.xar`): referenced, but not rendered.
    pub images_unrenderable: usize,
    /// Bitmap objects with photo operations (W10.6).
    pub photo_ops: usize,
    /// Interchange only: bitmap objects with photo operations shown
    /// unadjusted, because no [`SvgOptions::derived_bitmaps`] baked them.
    pub photo_ops_unbaked: usize,
    pub texts: usize,
    pub characters: usize,
    /// Stories on a path the base SVG shows on straight lines: no
    /// [`TextPlacer`] placed them along the path (it cannot when the
    /// characters are reflected or sheared). The others have each
    /// character placed and turned on the path (T9.5.6).
    pub text_on_path: usize,
    pub clips: usize,
    /// ClipViews whose clipping shape is not a path or shape.
    pub clips_unsupported: usize,
    pub live: usize,
    pub opaque: usize,
    pub gradients: usize,
    pub masks: usize,
    /// Ramps with a profile, easing or HSV interpolation, baked into stops.
    pub ramps_baked: usize,
    /// Fills SVG cannot draw (conical, diamond, 3/4-colour, fractal, noise,
    /// bitmap transparency), written as an approximation plus their twin.
    pub fills_approximated: usize,
    /// Conical and three- and four-colour fills and transparencies drawn
    /// as fine SVG geometry within a few levels of the model (`bake.rs`),
    /// plus their twin. Diamonds are baked exactly and not counted.
    pub fills_baked: usize,
    /// Perspective gradients drawn as their affine part.
    pub perspective_approximated: usize,
    pub blend_modes: usize,
    /// Blend modes with no CSS keyword (contrast, brightness).
    pub blend_modes_approximated: usize,
    /// Arrowheads recorded but not drawn (markers are not written yet).
    pub arrows_unbaked: usize,
    /// Variable-width and brush strokes, drawn as plain strokes.
    pub strokes_approximated: usize,
    /// Feathering recorded but not drawn (a text run's).
    pub effects_approximated: usize,
    /// Feathers drawn by an SVG filter (`effect.rs`), plus their twin.
    pub effects_baked: usize,
    /// Shadows recorded (`<xarast:shadow>`) but not baked: an external
    /// viewer draws the object without its shadow (a controller without a
    /// source, which a valid document never has).
    pub shadows_unbaked: usize,
    /// Shadows drawn by an SVG filter (`effect.rs`), on the controller or,
    /// for floor shadows and feathered controllers, on a derived copy of
    /// the object, plus their twin.
    pub shadows_baked: usize,
    /// Bevels recorded (`<xarast:bevel>`) but not baked (their controller
    /// also owns a feather): an external viewer draws the object without
    /// its bevel.
    pub bevels_unbaked: usize,
    /// Bevels drawn by an SVG lighting filter (`effect.rs`).
    pub bevels_baked: usize,
    /// Contour rings baked as real offset paths (§6.8.3), the start ring
    /// of each contour included even when it is empty.
    pub contour_rings: usize,
    /// Blend steps baked as real paths (§6.8.4).
    pub blend_steps: usize,
    /// Outlines baked into moulds' generated groups (`research/06 §6.8.5`).
    pub moulded_paths: usize,
    /// Text stories under a mould left out of its baked copy, for want of
    /// a placer that makes outlines.
    pub moulded_text_unbaked: usize,
    /// `ClipRegion` attributes, which nothing draws yet.
    pub clip_regions_ignored: usize,
    /// Non-attribute children of ink nodes, drawn before them as siblings.
    pub ink_children: usize,
    /// Foreign items written back (attributes and fragments).
    pub foreign_items: usize,
    /// Foreign items that could not be written (bad name, malformed
    /// fragment, clash with a known attribute).
    pub foreign_dropped: usize,
    /// Interchange: nodes whose foreign baggage (preserved unknown data)
    /// was not written.
    pub foreign_omitted: usize,
    /// Interchange: `xarast:` elements the projection removed.
    pub private_elements: usize,
    /// Interchange: `xarast:` attributes the projection removed.
    pub private_attributes: usize,
    /// `<defs>` requests satisfied by an identical definition.
    pub defs_deduplicated: usize,
    /// Pass 4: paint attributes removed from children because their `<g>`
    /// now carries them.
    pub paint_hoisted: usize,
    /// Pass 5: CSS paint classes written.
    pub paint_classes: usize,
    /// Pass 5: elements (ink or `<g>`) whose paint is a class.
    pub paint_classed: usize,
    /// Faces embedded as WOFF2 with an `@font-face` rule
    /// (`research/06 §6.7` rule 2).
    pub fonts_embedded: usize,
    /// Faces not embedded because their licence forbids it (rule 3).
    pub fonts_denied: usize,
    /// Faces that could not be embedded for another reason.
    pub fonts_failed: usize,
    /// Size of the SVG text in bytes.
    pub bytes: usize,
}

/// What became of one face the text is drawn with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontOutcome {
    /// The face.
    pub face: PlacedFace,
    /// `None` when embedded; otherwise why not.
    pub not_embedded: Option<Arc<str>>,
    /// Whether it was refused by its licence (`fsType`).
    pub denied: bool,
}

/// The result of [`write_svg`].
#[derive(Debug, Clone)]
pub struct SvgDocument {
    /// The text of `document.svg`.
    pub svg: String,
    /// What was written and what was approximated.
    pub stats: Stats,
    /// How many foreign items were written back (`xarast:foreign-count`).
    pub foreign_count: usize,
    /// The preservation digest of those items (`xarast:foreign-digest`).
    pub foreign_digest: [u8; 32],
    /// Every face the placed text is drawn with, in `@font-face` order,
    /// and whether it was embedded.
    pub fonts: Vec<FontOutcome>,
}

/// The package entry a bitmap resource is written as.
fn bitmap_entry(
    res: &xarast_doc::BitmapResource,
) -> Option<(ResourceKind, &'static str, std::sync::Arc<[u8]>, bool)> {
    let o = res.original.as_ref()?;
    let (kind, ext, renderable) = match o.format {
        ImageFormat::Png => (ResourceKind::Image, "png", true),
        ImageFormat::Jpeg => (ResourceKind::Image, "jpg", true),
        ImageFormat::Gif => (ResourceKind::Image, "gif", true),
        // Kept byte for byte (re-encoding would lose fidelity), but no
        // browser-grade renderer is guaranteed to decode them: Phase 10
        // adds a PNG rendition.
        ImageFormat::Bmp | ImageFormat::Unknown => (ResourceKind::Blob, "bin", false),
    };
    Some((kind, ext, std::sync::Arc::clone(&o.bytes), renderable))
}

/// The most entries a bitmap palette resource may hold.
pub(crate) const MAX_PALETTE_ENTRIES: usize = 256;

/// A bitmap's reconstruction palette as its resource bytes: `r g b a` per
/// entry, in order (`research/06 §6.9`). `None` for no palette (or an
/// oversized one, which no importer produces).
pub(crate) fn palette_bytes(palette: &[xarast_color::Rgba8]) -> Option<Vec<u8>> {
    if palette.is_empty() || palette.len() > MAX_PALETTE_ENTRIES {
        return None;
    }
    Some(palette.iter().flat_map(|c| [c.r, c.g, c.b, c.a]).collect())
}

/// The inverse of [`palette_bytes`]: `None` unless the bytes are 1–256
/// whole entries.
pub(crate) fn palette_from_bytes(bytes: &[u8]) -> Option<Vec<xarast_color::Rgba8>> {
    let (entries, rest) = bytes.as_chunks::<4>();
    if !rest.is_empty() || entries.is_empty() || entries.len() > MAX_PALETTE_ENTRIES {
        return None;
    }
    Some(
        entries
            .iter()
            .map(|&[r, g, b, a]| xarast_color::Rgba8 { r, g, b, a })
            .collect(),
    )
}

/// Serialises a document as `document.svg`, adding the bitmaps it
/// references to `resources` (one reference per `href` written).
pub fn write_svg(doc: &Document, resources: &mut ResourceIndex, opts: &SvgOptions) -> SvgDocument {
    // The first spread frames the root viewBox (§5.8.1).
    let first_spread = doc
        .tree
        .preorder(doc.tree.root())
        .find_map(|n| match doc.tree.kind(n) {
            Some(NodeKind::Spread(s)) => Some(s.page_size),
            _ => None,
        });
    let page = first_spread.unwrap_or_else(|| xarast_doc::SpreadNode::default().page_size);
    let frame = frame::Frame {
        ox: i64::from(page.lo.x.raw()),
        oy: i64::from(page.hi.y.raw()),
    };
    // The root viewBox, in the first spread's frame.
    let framed = opts.area.unwrap_or(page);
    let view = ViewBox {
        x: i64::from(framed.lo.x.raw()) - frame.ox,
        y: frame.oy - i64::from(framed.hi.y.raw()),
        w: i64::from(framed.hi.x.raw()) - i64::from(framed.lo.x.raw()),
        h: i64::from(framed.hi.y.raw()) - i64::from(framed.lo.y.raw()),
    };

    let mut cache: HashMap<BitmapId, Option<paint::BitmapRef>> = HashMap::new();
    let mut unrenderable = 0usize;
    let mut href = |id: BitmapId| -> Option<paint::BitmapRef> {
        if let Some(hit) = cache.get(&id) {
            if let Some(r) = hit
                && opts.bitmaps.is_none()
            {
                resources.count_path(&r.href);
                if let Some(p) = &r.palette {
                    resources.count_path(p);
                }
            }
            return hit.clone();
        }
        let res = doc.resources.bitmap(id)?;
        if let Some(link) = &opts.bitmaps {
            let out = (link.0)(id, res).map(|href| paint::BitmapRef {
                href,
                width: res.info.width,
                height: res.info.height,
                palette: None,
            });
            cache.insert(id, out.clone());
            return out;
        }
        let out = bitmap_entry(res).and_then(|(kind, ext, bytes, renderable)| {
            let rid = resources.insert(kind, ext, bytes).ok()?;
            let path = resources
                .get(rid)
                .map_or_else(|| resource_path(kind, rid, ext), |r| r.path());
            if !renderable {
                unrenderable += 1;
            }
            // The reconstruction palette of a `.xar` JPEG8BPP bitmap: data
            // only Xarast reads, so a blob beside the (browser-readable)
            // image rather than inside it.
            let palette = palette_bytes(&res.pixels.palette).and_then(|b| {
                let pid = resources.insert(ResourceKind::Blob, "bin", b).ok()?;
                Some(resources.get(pid).map_or_else(
                    || resource_path(ResourceKind::Blob, pid, "bin"),
                    |r| r.path(),
                ))
            });
            Some(paint::BitmapRef {
                href: path,
                width: res.info.width,
                height: res.info.height,
                palette,
            })
        });
        cache.insert(id, out.clone());
        out
    };

    let mut e = emit::Emitter::new(doc, frame, opts, &mut href);
    e.document();
    e.styler.plan();
    // The embedded fonts: one WOFF2 file and one `@font-face` rule per
    // face, with every character the document draws in it.
    let mut font_css = String::new();
    let mut fonts = Vec::new();
    // Stored once the emitter (which borrows the index through the bitmap
    // closure) is gone; the name is the content hash's either way.
    let mut font_files: Vec<Arc<[u8]>> = Vec::new();
    if let Some(placer) = e.placer.clone() {
        for (face, chars) in std::mem::take(&mut e.fonts) {
            let chars: Vec<char> = chars.into_iter().collect();
            let (not_embedded, denied) = match placer.0.font_file(&face, &chars) {
                None => continue,
                Some(FontFile::Woff2(bytes)) => {
                    let href = if opts.dialect == SvgDialect::Interchange {
                        Some(format!("data:font/woff2;base64,{}", xml::base64(&bytes)))
                    } else {
                        let path =
                            resource_path(ResourceKind::Font, ResourceId::of(&bytes), "woff2");
                        font_files.push(bytes);
                        Some(path)
                    };
                    match href {
                        Some(h) => {
                            font_css.push_str(&text::font_face_rule(&face, &h));
                            font_css.push('\n');
                            (None, false)
                        }
                        None => (Some(Arc::from("the resource could not be stored")), false),
                    }
                }
                Some(FontFile::Denied(why)) => (Some(why), true),
                Some(FontFile::Unavailable(why)) => (Some(why), false),
            };
            match (&not_embedded, denied) {
                (None, _) => e.stats.fonts_embedded += 1,
                (Some(_), true) => e.stats.fonts_denied += 1,
                (Some(_), false) => e.stats.fonts_failed += 1,
            }
            fonts.push(FontOutcome {
                face,
                not_embedded,
                denied,
            });
        }
    }
    let mut stats = std::mem::take(&mut e.stats);
    stats.defs_deduplicated = e.defs.hits;
    stats.paint_hoisted = e.styler.stats.hoisted;
    stats.paint_classes = e.styler.stats.classes;
    stats.paint_classed = e.styler.stats.classed;
    let digest: [u8; 32] = e.foreign_hash.finalize().into();
    let count = e.foreign_count;
    let svg = assemble(&e, doc, view, opts, count, &digest, &font_css);
    drop(e);
    for bytes in font_files {
        let _ = resources.insert(ResourceKind::Font, "woff2", bytes);
    }
    let svg = if opts.dialect == SvgDialect::Interchange {
        let (svg, p) = interchange::project(&svg, opts.minify);
        stats.private_elements = p.elements;
        stats.private_attributes = p.attributes;
        svg
    } else {
        svg
    };
    stats.images_unrenderable = unrenderable;
    stats.bitmaps = cache.values().filter(|r| r.is_some()).count();
    stats.bytes = svg.len();
    SvgDocument {
        svg,
        stats,
        foreign_count: count,
        foreign_digest: digest,
        fonts,
    }
}

/// The root `viewBox`, in integer millipoints of the first spread's frame.
#[derive(Debug, Clone, Copy)]
struct ViewBox {
    x: i64,
    y: i64,
    w: i64,
    h: i64,
}

/// Header, `<defs>`, named view, body.
fn assemble(
    e: &emit::Emitter<'_, '_>,
    doc: &Document,
    view: ViewBox,
    opts: &SvgOptions,
    foreign_count: usize,
    digest: &[u8; 32],
    font_css: &str,
) -> String {
    use num::{f64s, mp};
    use xml::{attr, push_text_escaped};

    let mut s = String::with_capacity(e.body.len() + 4096);
    s.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<svg");
    attr(&mut s, "xmlns", NS_SVG);
    attr(&mut s, "xmlns:xlink", NS_XLINK);
    attr(&mut s, "xmlns:xarast", NS_XARAST);
    attr(&mut s, "xmlns:inkscape", NS_INKSCAPE);
    attr(&mut s, "xmlns:sodipodi", NS_SODIPODI);
    attr(&mut s, "xmlns:dc", NS_DC);
    attr(&mut s, "xmlns:cc", NS_CC);
    attr(&mut s, "xmlns:rdf", NS_RDF);
    if opts.dialect == SvgDialect::Native {
        for (uri, prefix) in &e.ns.foreign {
            attr(&mut s, &format!("xmlns:{prefix}"), uri);
        }
    }
    let to_mm = |v: i64| v as f64 / 1000.0 / 72.0 * 25.4;
    attr(&mut s, "width", &format!("{}mm", f64s(to_mm(view.w), 3)));
    attr(&mut s, "height", &format!("{}mm", f64s(to_mm(view.h), 3)));
    attr(
        &mut s,
        "viewBox",
        &format!(
            "{} {} {} {}",
            mp(view.x),
            mp(view.y),
            mp(view.w),
            mp(view.h)
        ),
    );
    attr(&mut s, "version", "1.1");
    // The root node's own id and baggage.
    s.push_str(&e.root_attrs);
    s.push_str(">\n");

    if let Some(t) = doc.meta.title.as_deref().filter(|t| !t.is_empty()) {
        s.push_str("<title>");
        push_text_escaped(&mut s, t);
        s.push_str("</title>\n");
    }
    s.push_str("<metadata><rdf:RDF><cc:Work rdf:about=\"\"><dc:format>image/svg+xml</dc:format>");
    if let Some(t) = doc.meta.title.as_deref().filter(|t| !t.is_empty()) {
        s.push_str("<dc:title>");
        push_text_escaped(&mut s, t);
        s.push_str("</dc:title>");
    }
    if let Some(d) = doc.meta.modified.or(doc.meta.created) {
        s.push_str("<dc:date>");
        s.push_str(&crate::time::rfc3339_unix(d));
        s.push_str("</dc:date>");
    }
    if let Some(p) = doc.meta.producer.as_deref().filter(|t| !t.is_empty()) {
        s.push_str("<dc:source>");
        push_text_escaped(&mut s, p);
        s.push_str("</dc:source>");
    }
    s.push_str("</cc:Work></rdf:RDF></metadata>\n<defs>\n");
    if !font_css.is_empty() {
        s.push_str("<style type=\"text/css\">\n");
        s.push_str(font_css);
        s.push_str("</style>\n");
    }
    let style = e.styler.style_element();
    if !style.is_empty() {
        s.push_str(&style);
        s.push('\n');
    }

    s.push_str("<xarast:document");
    attr(&mut s, "xarast:version", "1.0");
    attr(&mut s, "xarast:min-reader", "1.0");
    attr(&mut s, "xarast:y-axis", "down");
    attr(&mut s, "xarast:layout", "single");
    attr(&mut s, "xarast:colour-refs", "literal");
    if let Some(NodeKind::Document(d)) = doc.tree.kind(doc.tree.root())
        && d.multi_chapter
    {
        attr(&mut s, "xarast:multi-chapter", "true");
    }
    if foreign_count > 0 {
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        attr(&mut s, "xarast:foreign-digest", &format!("blake3:{hex}"));
        attr(&mut s, "xarast:foreign-count", &foreign_count.to_string());
    }
    if e.header.chapters.is_empty() {
        s.push_str("/>\n");
    } else {
        s.push_str(">\n");
        for c in &e.header.chapters {
            s.push_str("<xarast:chapter");
            attr(&mut s, "xarast:id", &c.id);
            attr(&mut s, "xarast:spreads", &c.spreads.join(" "));
            s.push_str("/>\n");
        }
        s.push_str("</xarast:document>\n");
    }
    let pal = emit::palette_xml(doc, &e.palette);
    if !pal.is_empty() {
        s.push_str(&pal);
        s.push('\n');
    }
    for d in e.defs.items() {
        s.push_str(d);
        s.push('\n');
    }
    s.push_str("</defs>\n");

    s.push_str("<sodipodi:namedview");
    attr(&mut s, "id", "base");
    attr(&mut s, "inkscape:document-units", "mm");
    if let Some(l) = &e.header.active_layer {
        attr(&mut s, "inkscape:current-layer", l);
    }
    if e.header.grid.is_none() && e.header.guides.is_empty() {
        s.push_str("/>\n");
    } else {
        s.push('>');
        if let Some(g) = &e.header.grid {
            s.push_str(g);
        }
        for g in &e.header.guides {
            s.push_str("<sodipodi:guide");
            if g.horizontal {
                attr(&mut s, "position", &format!("0,{}", mp(g.position)));
                attr(&mut s, "orientation", "0,1");
            } else {
                attr(&mut s, "position", &format!("{},0", mp(g.position)));
                attr(&mut s, "orientation", "1,0");
            }
            s.push_str("/>");
        }
        s.push_str("</sodipodi:namedview>\n");
    }
    if let Some(c) = opts
        .background
        .filter(|_| opts.dialect == SvgDialect::Interchange)
    {
        s.push_str("<rect");
        attr(&mut s, "x", &mp(view.x));
        attr(&mut s, "y", &mp(view.y));
        attr(&mut s, "width", &mp(view.w));
        attr(&mut s, "height", &mp(view.h));
        attr(
            &mut s,
            "fill",
            &format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b),
        );
        if c.a < 255 {
            attr(&mut s, "fill-opacity", &f64s(f64::from(c.a) / 255.0, 4));
        }
        s.push_str("/>\n");
    }
    e.styler.write_body(&e.body, &mut s);
    s.push_str("</svg>\n");
    s
}
