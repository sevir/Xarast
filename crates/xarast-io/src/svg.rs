//! SVG export (phase 11 W11.3): the `.xarast` profile's mapper in its
//! `Interchange` dialect.
//!
//! There is one SVG mapper in Xarast, `xarast_format::svg::write_svg`.
//! `.xarast` calls it with `SvgDialect::Native`; this exporter calls it
//! with `SvgDialect::Interchange`, which writes the same base SVG without
//! the `xarast:` parametric layer and without preserved foreign data, and
//! with bitmaps linked the way the options say (inline `data:` URIs or a
//! sidecar folder) instead of into a package. The export area frames the
//! root `viewBox`; coordinates are the profile's own (points, y down, the
//! first spread's frame), so an SVG of the drawing and one of the page
//! differ only in the root element.
//!
//! Everything the SVG cannot carry is in the report: the writer's
//! approximation counters become [`Compromise`]s. Fonts are embedded
//! (T11.3.4): with the application's text placer, every face the text is
//! drawn with goes into the file as a WOFF2 subset in a `data:` URI behind
//! an `@font-face` rule — the same subsets `.xarast` stores — except a face
//! whose licence forbids it, which is reported as `FontNotEmbedded`.
//! Without a placer nothing is embedded and every family is reported.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use xarast_doc::BitmapResource;
use xarast_doc::resources::ImageFormat;
use xarast_format::ResourceIndex;
use xarast_format::svg::{self as fsvg, BitmapLinker, DerivedLinker, SvgDialect};

use crate::model::{Background, ExportRequest};
use crate::options::{
    FormatId, FormatOptions, PngColour, PngDepth, SvgOptions, SvgResources, TextOutput,
};
use crate::png::{PngHeader, encode_png, with_icc_profile};
use crate::raster::AtomicFile;
use crate::registry::{Capabilities, Exporter};
use crate::report::{Compromise, ExportError, ExportReport};
use crate::source::{ExportSource, Progress, Stage};

/// SVG 1.1, interchange dialect.
#[derive(Debug, Clone, Copy, Default)]
pub struct SvgExporter;

impl SvgExporter {
    fn options(req: &ExportRequest) -> Result<SvgOptions, ExportError> {
        match req.options {
            FormatOptions::Svg(o) => Ok(o),
            _ => Err(ExportError::BadOptions {
                format: FormatId::Svg,
                reason: "options are not SVG options".into(),
            }),
        }
    }
}

impl Exporter for SvgExporter {
    fn id(&self) -> FormatId {
        FormatId::Svg
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["svg"]
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            vector: true,
            alpha: true,
            multipage: false,
            // WOFF2 subsets in `@font-face` rules (T11.3.4), where the
            // face's licence allows.
            embeds_fonts: true,
            has_dpi: false,
            lossy: false,
            deterministic: true,
            max_side: u32::MAX,
            streams: false,
        }
    }

    fn export(
        &self,
        src: &dyn ExportSource,
        req: &ExportRequest,
        progress: &dyn Progress,
    ) -> Result<ExportReport, ExportError> {
        let t0 = Instant::now();
        let o = Self::options(req)?;
        let doc = src.document().ok_or(ExportError::UnsupportedFormat {
            id: FormatId::Svg.name().to_owned(),
            reason: "SVG export maps the document, and this source has only a scene",
        })?;
        // Text as outlines: every story (`TextOutput::Outlines`), or only
        // the stories drawn with a face whose licence forbids embedding
        // (T9.6.5), on a copy the application converts.
        let outlined = src.text_as_outlines(o.text == TextOutput::Outlines);
        let (doc, refused): (&xarast_doc::Document, &[Arc<str>]) = match &outlined {
            Some((d, f)) => (d, f),
            None => (doc, &[]),
        };
        let area = req.bled(src.resolve_area(&req.area)?)?;
        if progress.cancelled() {
            return Err(ExportError::Cancelled);
        }
        progress.report(Stage::Encode, 0.0);
        let sidecar = match o.resources {
            SvgResources::Inline => None,
            SvgResources::Sidecar => Some(sidecar_dir(&req.destination)),
        };
        let links = Arc::new(Mutex::new(Links::default()));
        let linker = {
            let links = Arc::clone(&links);
            let prefix = sidecar.as_ref().map(|(_, href)| href.clone());
            BitmapLinker(Arc::new(move |_, res: &BitmapResource| {
                let mut l = links.lock().unwrap_or_else(PoisonError::into_inner);
                l.link(res, prefix.as_deref())
            }))
        };
        let background = match req.background {
            Background::Transparent => None,
            Background::Paper => Some(src.paper_colour()),
            Background::Colour(c) => Some(c),
        };
        // A bitmap object with photo operations shows its adjusted pixels
        // (W10.6): the chain is baked into a PNG of its own, since an SVG
        // viewer cannot evaluate it.
        let derived = {
            let links = Arc::clone(&links);
            let prefix = sidecar.as_ref().map(|(_, href)| href.clone());
            DerivedLinker(Arc::new(move |_, res: &BitmapResource, ops| {
                let (bytes, mime, ext) = baked_image(res, ops)?;
                let mut l = links.lock().unwrap_or_else(PoisonError::into_inner);
                Some(l.link_bytes(bytes, mime, ext, prefix.as_deref()))
            }))
        };
        let opts = fsvg::SvgOptions {
            pretty: o.pretty && !o.minify,
            text: src.svg_text_placer(),
            dialect: SvgDialect::Interchange,
            area: Some(area),
            background,
            minify: o.minify,
            bitmaps: Some(linker),
            derived_bitmaps: Some(derived),
            ..fsvg::SvgOptions::default()
        };
        let t = Instant::now();
        let out = fsvg::write_svg(doc, &mut ResourceIndex::new(), &opts);
        // "Render" is the mapping: SVG has no rasterising step.
        let mut report = ExportReport {
            render_time: t.elapsed(),
            ..ExportReport::default()
        };
        progress.report(Stage::Encode, 0.5);
        if progress.cancelled() {
            return Err(ExportError::Cancelled);
        }
        let links = std::mem::take(&mut *links.lock().unwrap_or_else(PoisonError::into_inner));
        let te = Instant::now();
        if let Some((dir, _)) = &sidecar
            && !links.files.is_empty()
        {
            std::fs::create_dir_all(dir)?;
            for (name, bytes) in &links.files {
                if progress.cancelled() {
                    return Err(ExportError::Cancelled);
                }
                report.bytes_written += AtomicFile::new(&dir.join(name)).write_all(bytes)?;
            }
        }
        report.bytes_written += AtomicFile::new(&req.destination).write_all(out.svg.as_bytes())?;
        report.encode_time = te.elapsed();
        progress.report(Stage::Encode, 1.0);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        {
            let per_pt = f64::from(xarast_geom::Mp::PER_PT);
            report.pixels = (
                (area.width().to_f64() / per_pt).round() as u32,
                (area.height().to_f64() / per_pt).round() as u32,
            );
        }
        report.dpi = 72.0;
        report.commands = out.stats.elements;
        report.compromises = compromises(&out.stats, &out.svg, links.failed);
        if opts.text.is_some() {
            // The families the file names are embedded (or substituted by
            // an embedded face) except the ones the writer could not embed.
            report
                .compromises
                .retain(|c| !matches!(c, Compromise::FontNotEmbedded { .. }));
            for family in refused {
                report.compromises.push(Compromise::FontNotEmbedded {
                    family: Arc::clone(family),
                    reason: "its licence (OS/2 fsType) forbids embedding; its text is \
                             written as outlines"
                        .into(),
                });
            }
            for f in out.fonts.iter().filter(|f| f.not_embedded.is_some()) {
                let c = Compromise::FontNotEmbedded {
                    family: Arc::clone(&f.face.family),
                    reason: f.not_embedded.clone().unwrap_or_else(|| Arc::from("")),
                };
                if !report.compromises.contains(&c) {
                    report.compromises.push(c);
                }
            }
        }
        report
            .compromises
            .extend(crate::fidelity::document_compromises(
                src,
                crate::fidelity::Target::Svg,
            ));
        report.duration = t0.elapsed();
        Ok(report)
    }
}

/// The sidecar folder for a destination (`<stem>_files` beside it) and
/// the relative `href` prefix that names it. Characters outside
/// `[A-Za-z0-9._-]` become `_`, so the folder name is its own URI
/// reference: some renderers (resvg among them) do not percent-decode
/// relative paths.
fn sidecar_dir(dest: &Path) -> (PathBuf, String) {
    let stem = dest
        .file_stem()
        .map_or_else(|| "export".to_owned(), |s| s.to_string_lossy().into_owned());
    let name: String = format!("{stem}_files")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "-._".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let href = format!("{name}/");
    (dest.with_file_name(name), href)
}

/// The bitmaps an export links, in the order the writer asks for them.
#[derive(Debug, Default)]
struct Links {
    /// Encoded bytes → the `href` already written for them.
    seen: HashMap<Vec<u8>, String>,
    /// Sidecar files: name and bytes.
    files: Vec<(String, Vec<u8>)>,
    /// Bitmaps that could not be written.
    failed: usize,
}

impl Links {
    /// The `href` for a bitmap: its original bytes when a browser reads
    /// them as they are, otherwise its pixels as a PNG.
    fn link(&mut self, res: &BitmapResource, sidecar: Option<&str>) -> Option<String> {
        let Some((bytes, mime, ext)) = browser_image(res) else {
            self.failed += 1;
            return None;
        };
        Some(self.link_bytes(bytes, mime, ext, sidecar))
    }

    /// The `href` for encoded image bytes, written once however many
    /// times they are linked.
    fn link_bytes(
        &mut self,
        bytes: Vec<u8>,
        mime: &str,
        ext: &str,
        sidecar: Option<&str>,
    ) -> String {
        if let Some(h) = self.seen.get(&bytes) {
            return h.clone();
        }
        let href = match sidecar {
            None => format!("data:{mime};base64,{}", fsvg::xml::base64(&bytes)),
            Some(prefix) => {
                let name = format!("image-{}.{ext}", self.files.len() + 1);
                let href = format!("{prefix}{name}");
                self.files.push((name, bytes.clone()));
                href
            }
        };
        self.seen.insert(bytes, href.clone());
        href
    }
}

/// A bitmap object's photo-adjusted image as a PNG: the master's pixels
/// (as [`pixels`] reads them) through the chain
/// ([`crate::photo::bake`], the evaluation the renderer uses). The
/// master's ICC profile still describes the result, so it travels too.
fn baked_image(
    res: &BitmapResource,
    ops: &xarast_doc::PhotoOps,
) -> Option<(Vec<u8>, &'static str, &'static str)> {
    let (w, h, rgba, icc) = pixels(res)?;
    let (w, h, rgba) = crate::photo::bake(w, h, &rgba, ops)?;
    let header = PngHeader {
        width: w,
        height: h,
        colour: PngColour::Rgba,
        depth: PngDepth::Eight,
        interlace: false,
        ppm: None,
        level: 6,
    };
    let mut png = Vec::new();
    encode_png(&mut png, header, &rgba).ok()?;
    if let Some(tagged) = icc.and_then(|p| with_icc_profile(&png, &p)) {
        png = tagged;
    }
    Some((png, "image/png", "png"))
}

/// A bitmap as bytes every SVG viewer decodes: PNG, JPEG and GIF
/// originals pass through untouched; anything else (the `.xar` BMP
/// flavours, a JPEG that needs its reconstruction palette, a bitmap with
/// only pixels) is decoded as the renderer's walker decodes it and written
/// as an RGBA PNG.
fn browser_image(res: &BitmapResource) -> Option<(Vec<u8>, &'static str, &'static str)> {
    if let Some(o) = &res.original
        && res.pixels.palette.is_empty()
    {
        match o.format {
            ImageFormat::Png => return Some((o.bytes.to_vec(), "image/png", "png")),
            ImageFormat::Jpeg => return Some((o.bytes.to_vec(), "image/jpeg", "jpg")),
            ImageFormat::Gif => return Some((o.bytes.to_vec(), "image/gif", "gif")),
            ImageFormat::Bmp | ImageFormat::Unknown => {}
        }
    }
    let (w, h, rgba, icc) = pixels(res)?;
    let header = PngHeader {
        width: w,
        height: h,
        colour: PngColour::Rgba,
        depth: PngDepth::Eight,
        interlace: false,
        ppm: None,
        level: 6,
    };
    let mut png = Vec::new();
    encode_png(&mut png, header, &rgba).ok()?;
    // The pixels were decoded without converting them, so the profile
    // still describes them: it travels in `iCCP` (T11.5.2).
    if let Some(tagged) = icc.and_then(|p| with_icc_profile(&png, &p)) {
        png = tagged;
    }
    Some((png, "image/png", "png"))
}

/// Width, height, straight RGBA8 samples and the embedded ICC profile.
type Pixels = (u32, u32, Vec<u8>, Option<Arc<[u8]>>);

/// Straight RGBA8 pixels of a bitmap, by the same rules as the scene
/// walker: stored pixels of the right size as they are, otherwise the
/// original decoded (`.xar` tag 71 with its palette, 65 for BMP, 69 for
/// the importer's compressed BMP). Also the ICC profile the original
/// embeds, if any.
fn pixels(res: &BitmapResource) -> Option<Pixels> {
    let (w, h) = (res.info.width, res.info.height);
    if w > 0 && h > 0 && res.pixels.pixels.len() == w as usize * h as usize * 4 {
        // Decoding again only for the profile, and only when there is one.
        let icc = if crate::fidelity::has_icc_profile(res) {
            decode_original(res).and_then(|d| d.icc)
        } else {
            None
        };
        return Some((w, h, res.pixels.pixels.to_vec(), icc));
    }
    let d = decode_original(res)?;
    let icc = d.icc.clone();
    let d = d.data;
    (d.width > 0 && d.height > 0).then(|| (d.width, d.height, d.to_straight_rgba8(), icc))
}

/// The original bytes decoded by the walker's rules.
fn decode_original(res: &BitmapResource) -> Option<xarast_image::DecodedImage> {
    use xarast_image::xar::decode_xar_bitmap;
    let o = res.original.as_ref()?;
    let limits = xarast_image::DecodeLimits::default();
    match o.format {
        ImageFormat::Jpeg if !res.pixels.palette.is_empty() => {
            let palette: Vec<[u8; 3]> =
                res.pixels.palette.iter().map(|c| [c.r, c.g, c.b]).collect();
            decode_xar_bitmap(71, &o.bytes, &palette, &limits)
        }
        ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::Gif => {
            xarast_image::decode(&o.bytes, &limits)
        }
        ImageFormat::Bmp => decode_xar_bitmap(65, &o.bytes, &[], &limits),
        ImageFormat::Unknown => decode_xar_bitmap(69, &o.bytes, &[], &limits),
    }
    .ok()
}

/// The writer's counters as report entries, then the fonts the file
/// names and does not carry.
fn compromises(s: &fsvg::Stats, svg: &str, failed_images: usize) -> Vec<Compromise> {
    let not_rendered = [
        ("images (no pixels)", s.images_missing.max(failed_images)),
        (
            "photo adjustments (the image is shown unadjusted)",
            s.photo_ops_unbaked,
        ),
        ("quick shapes (no outline)", s.quickshapes_without_outline),
        ("clips (unsupported clip shape)", s.clips_unsupported),
        ("unknown .xar records", s.opaque),
        ("arrowheads", s.arrows_unbaked),
        ("feathering", s.effects_approximated),
        ("shadows (recorded, not baked)", s.shadows_unbaked),
        ("bevels (recorded, not baked)", s.bevels_unbaked),
    ];
    let simplified = [
        (
            "fills SVG cannot draw (conical, diamond, three- and four-colour, fractal, \
             noise, bitmap transparency) drawn as their nearest SVG paint",
            s.fills_approximated,
        ),
        (
            "perspective gradients drawn as their affine part",
            s.perspective_approximated,
        ),
        (
            "blend modes with no CSS keyword (contrast, brightness) drawn as normal",
            s.blend_modes_approximated,
        ),
        (
            "variable-width and brush strokes drawn as plain strokes",
            s.strokes_approximated,
        ),
        ("text on a path laid out on straight lines", s.text_on_path),
        (
            "photo adjustments baked into a PNG of the adjusted pixels",
            s.photo_ops.saturating_sub(s.photo_ops_unbaked),
        ),
        (
            "feathers drawn by an SVG filter (a Gaussian blur for the disc blur)",
            s.effects_baked,
        ),
        (
            "wall and glow shadows drawn by an SVG filter (a Gaussian blur for the disc blur)",
            s.shadows_baked,
        ),
        (
            "bevels drawn by an SVG lighting filter (a blurred height field for the faces)",
            s.bevels_baked,
        ),
    ];
    let mut out: Vec<Compromise> = not_rendered
        .into_iter()
        .filter(|(_, n)| *n > 0)
        .map(|(what, count)| Compromise::NotRendered {
            what: what.into(),
            count,
        })
        .chain(
            simplified
                .into_iter()
                .filter(|(_, n)| *n > 0)
                .map(|(what, count)| Compromise::Simplified {
                    what: what.into(),
                    count,
                }),
        )
        .collect();
    if s.foreign_omitted > 0 {
        out.push(Compromise::UnknownDataDropped {
            count: s.foreign_omitted,
        });
    }
    out.extend(
        font_families(svg)
            .into_iter()
            .map(|family| Compromise::FontNotEmbedded {
                family: family.into(),
                reason: "the SVG was written without the application's text layout, so no font is embedded; the viewer must have it"
                    .into(),
            }),
    );
    out
}

/// Every `font-family` value the file names, sorted.
fn font_families(svg: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for pat in ["font-family=\"", "font-family:"] {
        let mut from = 0usize;
        while let Some(k) = svg[from..].find(pat) {
            let start = from + k + pat.len();
            let end = svg[start..]
                .find(['"', ';', '}'])
                .map_or(svg.len(), |e| start + e);
            // The first family is the one asked for; the rest are the
            // writer's fallbacks.
            let first = svg[start..end].split(',').next().unwrap_or("");
            let v = first.trim().replace("&quot;", "").replace('\'', "");
            if !v.is_empty() {
                out.insert(v);
            }
            from = end;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidecar_hrefs_are_relative_and_escaped() {
        let (dir, href) = sidecar_dir(Path::new("/tmp/out/Fill Types #1.svg"));
        assert_eq!(dir, Path::new("/tmp/out/Fill_Types__1_files"));
        assert_eq!(href, "Fill_Types__1_files/");
    }

    #[test]
    fn font_families_are_collected_once() {
        let f = font_families(
            "<text font-family=\"Arial\"/><tspan font-family=\"'Times New Roman'\"/>\
             <text font-family=\"Arial, Liberation Sans, sans-serif\"/><style>.a{font-family:DejaVu Sans;}</style>",
        );
        let v: Vec<&str> = f.iter().map(String::as_str).collect();
        assert_eq!(v, ["Arial", "DejaVu Sans", "Times New Roman"]);
    }
}
