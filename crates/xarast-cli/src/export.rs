//! `xarast-cli export`: documents to PNG, JPEG, WebP, PDF or SVG through
//! the export filters of `xarast-io` (phase 11 T11.1.7, W11.3, W11.4).
//!
//! The CLI builds an [`ExportRequest`] from flags and hands it, with a
//! [`Session`] (an [`ExportSource`](xarast_io::ExportSource) of
//! `xarast-app`), to the same [`Registry`] the export dialog uses.
//! `.xar` is refused with the permanent reason of architecture §3.5.

use std::path::{Path, PathBuf};
use std::time::Instant;

use xarast_app::{DocumentId, Session};
use xarast_color::Rgba8;
use xarast_geom::{Mp, Point, Rect};
use xarast_io::{
    Background, BlendFidelity, ExportArea, ExportError, ExportRequest, ExportSizing, FormatId,
    FormatOptions, NoProgress, PDF_RASTERISE_DPI, PngColour, PngCompression, PngDepth, Registry,
    Subsampling, SvgResources, TextOutput, WebPMode, XAR_EXPORT_REFUSAL,
};
use xarast_render::RenderQuality;

use crate::Exit;
use crate::args::Args;
use crate::inputs::{expand, ms};

/// Usage for `export`.
pub const USAGE: &str = "\
xarast-cli export — export documents to PNG, JPEG, WebP, PDF or SVG

USAGE:
    xarast-cli export <IN.xar|IN.xarast> -o <OUT.png|.jpg|.webp|.pdf|.svg> [OPTIONS]
    xarast-cli export <IN|DIR>... --out-dir <DIR> --format <FMT> [OPTIONS]

WHAT AND HOW BIG
    --format FMT         png, jpeg, webp, pdf or svg (default: from -o's extension)
    --area WHAT          drawing (default; the page when nothing is drawn),
                         page, spread, or x0,y0,x1,y1 in points
    --bleed PT           grow the area by PT points on every side
    --dpi N              resolution; pixels follow the area (default 96)
    --width PX           pixel width; a missing height follows the aspect
    --height PX          pixel height; a missing width follows the aspect
    --background BG      transparent (default), paper, or RRGGBB[AA]
    --draft              draft render quality (default final)

PNG
    --depth 8|16         sample depth (default 8)
    --colour C           rgba (default), rgb, grey, grey-alpha, palette[:N]
    --interlace          Adam7 interlacing
    --compression C      fast, balanced (default) or best
    --optimise           oxipng pass (needs the `oxipng` feature)

JPEG
    --quality N          1-100 (default 90)
    --progressive        progressive rather than baseline
    --subsampling S      444, 422 or 420 (default 420)

WebP
    lossless only; --quality is refused (no pure-Rust lossy encoder)

PDF (one vector page the size of the area; --dpi and pixels do not apply)
    --raster-dpi N       resolution of objects PDF cannot express (default 300)
    --blend B            exact (default): rasterise every non-mix transparency
                         with its backdrop; native: Stained Glass as Multiply,
                         Bleach as Screen
    --no-compress        leave the streams readable

PDF and SVG
    --text T             text (default): live text, fonts embedded as subsets
                         where their licence (fsType) allows, outlines where
                         it does not; outlines: every story as glyph outlines

SVG (plain SVG 1.1 for browsers and Inkscape; the viewBox is the area)
    --resources R        inline (default): images as data: URIs;
                         sidecar: files in <OUT stem>_files/ beside the .svg
    --minify             drop unreferenced ids, comments and indentation
    --pretty             indent one space per level

COMMON
    --no-dpi             do not write the resolution into the file
    --quiet              print only failures and the summary

JPEG has no alpha: a transparent background is flattened onto the paper
colour, and the line for that file says so. `.xar` is never an export
format (docs/10-architecture.md §3.5).
";

/// Parsed `export` arguments.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportArgs {
    /// Files or directories.
    pub inputs: Vec<PathBuf>,
    /// `-o`.
    pub output: Option<PathBuf>,
    /// `--out-dir`.
    pub out_dir: Option<PathBuf>,
    /// The request every input shares; `destination` is set per input.
    pub request: ExportRequest,
    /// Print only failures and the summary.
    pub quiet: bool,
}

fn format_of(text: &str) -> Result<FormatId, String> {
    match text.trim_start_matches('.').to_ascii_lowercase().as_str() {
        "png" => Ok(FormatId::Png),
        "jpg" | "jpeg" | "jpe" | "jfif" => Ok(FormatId::Jpeg),
        "webp" => Ok(FormatId::WebP),
        "pdf" => Ok(FormatId::Pdf),
        "svg" => Ok(FormatId::Svg),
        "xar" | "web" => Err(XAR_EXPORT_REFUSAL.to_owned()),
        other => Err(format!(
            "`{other}` is not an export format (png, jpeg, webp, pdf, svg)"
        )),
    }
}

fn parse_area(text: &str) -> Result<ExportArea, String> {
    match text {
        "drawing" => Ok(ExportArea::Drawing),
        "page" => Ok(ExportArea::Page(None)),
        "spread" => Ok(ExportArea::Spread),
        "selection" => Ok(ExportArea::Selection),
        rect => {
            let v: Vec<f64> = rect
                .split(',')
                .map(|s| s.trim().parse::<f64>())
                .collect::<Result<_, _>>()
                .map_err(|_| {
                    format!("--area: `{rect}` is not drawing, page, spread or x0,y0,x1,y1")
                })?;
            match v[..] {
                [x0, y0, x1, y1] if v.iter().all(|x| x.is_finite()) => {
                    let r = Rect::new(
                        Point::new(Mp::from_pt(x0), Mp::from_pt(y0)),
                        Point::new(Mp::from_pt(x1), Mp::from_pt(y1)),
                    );
                    if r.width().raw() <= 0 || r.height().raw() <= 0 {
                        return Err("--area: the rectangle is empty".into());
                    }
                    Ok(ExportArea::Rect(r))
                }
                _ => Err(format!("--area: `{rect}` needs four numbers")),
            }
        }
    }
}

fn parse_background(text: &str) -> Result<Background, String> {
    match text {
        "transparent" | "none" => Ok(Background::Transparent),
        "paper" => Ok(Background::Paper),
        hex => {
            let h = hex.trim_start_matches('#');
            let byte = |i: usize| u8::from_str_radix(h.get(i..i + 2).unwrap_or("zz"), 16);
            let bad = || format!("--background: `{text}` is not transparent, paper or RRGGBB[AA]");
            match h.len() {
                6 | 8 => Ok(Background::Colour(Rgba8 {
                    r: byte(0).map_err(|_| bad())?,
                    g: byte(2).map_err(|_| bad())?,
                    b: byte(4).map_err(|_| bad())?,
                    a: if h.len() == 8 {
                        byte(6).map_err(|_| bad())?
                    } else {
                        255
                    },
                })),
                _ => Err(bad()),
            }
        }
    }
}

/// Every format-specific flag, gathered before the format is known.
#[derive(Debug, Default)]
struct FormatFlags {
    depth: Option<PngDepth>,
    colour: Option<PngColour>,
    interlace: bool,
    compression: Option<PngCompression>,
    optimise: bool,
    quality: Option<u8>,
    progressive: bool,
    subsampling: Option<Subsampling>,
    no_dpi: bool,
    raster_dpi: Option<u32>,
    blend: Option<BlendFidelity>,
    no_compress: bool,
    resources: Option<SvgResources>,
    minify: bool,
    pretty: bool,
    text: Option<TextOutput>,
}

impl FormatFlags {
    fn apply(&self, id: FormatId) -> Result<FormatOptions, String> {
        let png_only = self.depth.is_some()
            || self.colour.is_some()
            || self.interlace
            || self.compression.is_some()
            || self.optimise;
        let jpeg_only = self.progressive || self.subsampling.is_some();
        let mut o = FormatOptions::default_for(id);
        match &mut o {
            FormatOptions::Png(p) => {
                if jpeg_only || self.quality.is_some() {
                    return Err(
                        "--quality, --progressive and --subsampling are JPEG options".into(),
                    );
                }
                p.bit_depth = self.depth.unwrap_or(p.bit_depth);
                p.colour = self.colour.unwrap_or(p.colour);
                p.interlace = self.interlace;
                p.compression = self.compression.unwrap_or(p.compression);
                p.optimise = self.optimise;
                p.write_dpi = !self.no_dpi;
            }
            FormatOptions::Jpeg(j) => {
                if png_only {
                    return Err("--depth, --colour, --interlace, --compression and --optimise are PNG options".into());
                }
                j.quality = self.quality.unwrap_or(j.quality);
                j.progressive = self.progressive;
                j.subsampling = self.subsampling.unwrap_or(j.subsampling);
                j.write_dpi = !self.no_dpi;
            }
            FormatOptions::WebP(w) => {
                if png_only || jpeg_only {
                    return Err("WebP takes no PNG or JPEG options".into());
                }
                if let Some(q) = self.quality {
                    w.mode = WebPMode::Lossy { quality: q };
                }
            }
            FormatOptions::Pdf(p) => {
                if png_only || jpeg_only || self.quality.is_some() {
                    return Err("PDF takes no PNG or JPEG options".into());
                }
                p.rasterise_dpi = self.raster_dpi.unwrap_or(p.rasterise_dpi);
                p.blend_fidelity = self.blend.unwrap_or(p.blend_fidelity);
                p.compress = !self.no_compress;
                p.text = self.text.unwrap_or(p.text);
            }
            FormatOptions::Svg(v) => {
                if png_only || jpeg_only || self.quality.is_some() {
                    return Err("SVG takes no PNG or JPEG options".into());
                }
                v.resources = self.resources.unwrap_or(v.resources);
                v.minify = self.minify;
                v.pretty = self.pretty;
                v.text = self.text.unwrap_or(v.text);
            }
        }
        if self.text.is_some() && !matches!(id, FormatId::Pdf | FormatId::Svg) {
            return Err("--text is a PDF and SVG option".into());
        }
        let svg_only = self.resources.is_some() || self.minify || self.pretty;
        if svg_only && id != FormatId::Svg {
            return Err("--resources, --minify and --pretty are SVG options".into());
        }
        let pdf_only = self.raster_dpi.is_some() || self.blend.is_some() || self.no_compress;
        if pdf_only && id != FormatId::Pdf {
            return Err("--raster-dpi, --blend and --no-compress are PDF options".into());
        }
        Ok(o)
    }
}

/// Parses `export`'s arguments.
///
/// # Errors
///
/// A message for the user when the command line is wrong.
#[allow(clippy::too_many_lines)]
pub fn parse(argv: &[String]) -> Result<ExportArgs, String> {
    let mut inputs = Vec::new();
    let (mut output, mut out_dir) = (None::<PathBuf>, None::<PathBuf>);
    let mut format = None;
    let mut area = ExportArea::Drawing;
    let mut bleed = Mp::ZERO;
    let mut dpi = None;
    let (mut width, mut height) = (None::<u32>, None::<u32>);
    let mut background = Background::Transparent;
    let mut quality = RenderQuality::Final;
    let mut f = FormatFlags::default();
    let mut quiet = false;
    let mut it = Args::new(argv);
    while let Some(arg) = it.next_arg() {
        match arg.as_str() {
            "-o" | "--output" => output = Some(PathBuf::from(it.value(&arg)?)),
            "--out-dir" => out_dir = Some(PathBuf::from(it.value(&arg)?)),
            "--format" | "-f" => format = Some(format_of(&it.value(&arg)?)?),
            "--area" => area = parse_area(&it.value(&arg)?)?,
            "--bleed" => {
                let b: f64 = it.parsed(&arg)?;
                if !(b.is_finite() && b >= 0.0) {
                    return Err("--bleed must be zero or more points".into());
                }
                bleed = Mp::from_pt(b);
            }
            "--dpi" => {
                let d: f64 = it.parsed(&arg)?;
                if !(d.is_finite() && d > 0.0) {
                    return Err("--dpi must be a positive number".into());
                }
                dpi = Some(d);
            }
            "--width" => width = Some(it.parsed(&arg)?),
            "--height" => height = Some(it.parsed(&arg)?),
            "--background" | "--bg" => background = parse_background(&it.value(&arg)?)?,
            "--draft" => quality = RenderQuality::Draft,
            "--depth" => {
                f.depth = Some(match it.value(&arg)?.as_str() {
                    "8" => PngDepth::Eight,
                    "16" => PngDepth::Sixteen,
                    other => return Err(format!("--depth takes 8 or 16, not `{other}`")),
                });
            }
            "--colour" | "--color" => {
                let v = it.value(&arg)?;
                f.colour = Some(match v.as_str() {
                    "rgba" => PngColour::Rgba,
                    "rgb" => PngColour::Rgb,
                    "grey" | "gray" => PngColour::Grey,
                    "grey-alpha" | "gray-alpha" => PngColour::GreyAlpha,
                    "palette" => PngColour::Palette { max_colours: 256 },
                    p if p.starts_with("palette:") => {
                        let n: u16 = p["palette:".len()..]
                            .parse()
                            .map_err(|_| format!("--colour: `{p}` needs palette:N"))?;
                        if !(1..=256).contains(&n) {
                            return Err("--colour palette:N takes 1 to 256".into());
                        }
                        PngColour::Palette { max_colours: n }
                    }
                    other => return Err(format!("--colour: `{other}` is not a PNG colour type")),
                });
            }
            "--interlace" => f.interlace = true,
            "--compression" => {
                f.compression = Some(match it.value(&arg)?.as_str() {
                    "fast" => PngCompression::Fast,
                    "balanced" => PngCompression::Balanced,
                    "best" => PngCompression::Best,
                    other => {
                        return Err(format!(
                            "--compression: `{other}` is not fast, balanced or best"
                        ));
                    }
                });
            }
            "--optimise" | "--optimize" => f.optimise = true,
            "--quality" => {
                let q: u8 = it.parsed(&arg)?;
                if !(1..=100).contains(&q) {
                    return Err("--quality takes 1 to 100".into());
                }
                f.quality = Some(q);
            }
            "--progressive" => f.progressive = true,
            "--subsampling" => {
                f.subsampling = Some(match it.value(&arg)?.as_str() {
                    "444" | "4:4:4" => Subsampling::S444,
                    "422" | "4:2:2" => Subsampling::S422,
                    "420" | "4:2:0" => Subsampling::S420,
                    other => {
                        return Err(format!("--subsampling: `{other}` is not 444, 422 or 420"));
                    }
                });
            }
            "--no-dpi" => f.no_dpi = true,
            "--raster-dpi" => {
                let d: u32 = it.parsed(&arg)?;
                if !(PDF_RASTERISE_DPI.0..=PDF_RASTERISE_DPI.1).contains(&d) {
                    return Err(format!(
                        "--raster-dpi takes {} to {}",
                        PDF_RASTERISE_DPI.0, PDF_RASTERISE_DPI.1
                    ));
                }
                f.raster_dpi = Some(d);
            }
            "--blend" => {
                f.blend = Some(match it.value(&arg)?.as_str() {
                    "exact" => BlendFidelity::Exact,
                    "native" => BlendFidelity::PreferNative,
                    other => return Err(format!("--blend: `{other}` is not exact or native")),
                });
            }
            "--no-compress" => f.no_compress = true,
            "--resources" => {
                f.resources = Some(match it.value(&arg)?.as_str() {
                    "inline" => SvgResources::Inline,
                    "sidecar" => SvgResources::Sidecar,
                    other => {
                        return Err(format!("--resources: `{other}` is not inline or sidecar"));
                    }
                });
            }
            "--text" => {
                f.text = Some(match it.value(&arg)?.as_str() {
                    "text" => TextOutput::Text,
                    "outlines" => TextOutput::Outlines,
                    other => return Err(format!("--text: `{other}` is not text or outlines")),
                });
            }
            "--minify" => f.minify = true,
            "--pretty" => f.pretty = true,
            "--quiet" | "-q" => quiet = true,
            other if other.starts_with('-') && other.len() > 1 => {
                return Err(format!("unknown option {other}"));
            }
            other => inputs.push(PathBuf::from(other)),
        }
    }
    if inputs.is_empty() {
        return Err("no input given".into());
    }
    match (&output, &out_dir) {
        (None, None) => return Err("give -o FILE or --out-dir DIR".into()),
        (Some(_), Some(_)) => return Err("-o and --out-dir are exclusive".into()),
        (Some(_), None) if inputs.len() > 1 || inputs[0].is_dir() => {
            return Err("-o takes one input file; use --out-dir for several".into());
        }
        _ => {}
    }
    let id = match (format, &output) {
        (Some(id), Some(o)) => {
            // `.xar` is refused whatever the flag says.
            if let Some(ext) = o.extension().and_then(|e| e.to_str()) {
                let from_ext = format_of(ext);
                if let Err(e) = &from_ext
                    && e == XAR_EXPORT_REFUSAL
                {
                    return Err(e.clone());
                }
            }
            id
        }
        (Some(id), None) => id,
        (None, Some(o)) => format_of(o.extension().and_then(|e| e.to_str()).unwrap_or(""))?,
        (None, None) => return Err("--out-dir needs --format".into()),
    };
    let sizing = match (dpi, width, height) {
        (Some(_), Some(_), _) | (Some(_), _, Some(_)) => {
            return Err("give --dpi or --width/--height, not both".into());
        }
        (_, Some(0), _) | (_, _, Some(0)) => return Err("--width/--height must be positive".into()),
        (d, None, None) => ExportSizing::at_dpi(d.unwrap_or(96.0)),
        (None, w, h) => ExportSizing::with_pixels(w.unwrap_or(0), h.unwrap_or(0)),
    };
    let mut request = ExportRequest::new(f.apply(id)?, PathBuf::new());
    request.area = area;
    request.bleed = bleed;
    request.sizing = sizing;
    request.background = background;
    request.quality = quality;
    Ok(ExportArgs {
        inputs,
        output,
        out_dir,
        request,
        quiet,
    })
}

fn output_for(
    input: &Path,
    dir: &Path,
    ext: &str,
    used: &mut std::collections::HashSet<String>,
) -> PathBuf {
    let stem = input
        .file_stem()
        .map_or_else(|| "out".to_owned(), |s| s.to_string_lossy().into_owned());
    let mut name = format!("{stem}.{ext}");
    let mut n = 2;
    while !used.insert(name.clone()) {
        name = format!("{stem}-{n}.{ext}");
        n += 1;
    }
    dir.join(name)
}

/// Runs `export`.
#[must_use]
pub fn run(a: &ExportArgs) -> Exit {
    let files = match expand(&a.inputs) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("xarast-cli: {e}");
            return Exit::Import;
        }
    };
    let docs: Vec<Input<'_>> = files
        .into_iter()
        .map(|path| {
            let open: Opener<'_> =
                Box::new(|p: &Path| Session::open(DocumentId(1), p).map_err(|e| e.to_string()));
            (path, open)
        })
        .collect();
    run_on(a, docs)
}

/// One document to export: the name it is reported and written under,
/// and how to open it.
pub type Input<'a> = (PathBuf, Opener<'a>);

/// Opens one input into a session.
pub type Opener<'a> = Box<dyn Fn(&Path) -> Result<Session, String> + 'a>;

/// Exports documents that need not be files (`fixtures` builds them in
/// memory): each input's path names it in the output and the report, and
/// its closure opens it.
#[must_use]
pub fn run_on(a: &ExportArgs, files: Vec<Input<'_>>) -> Exit {
    if let Some(d) = &a.out_dir
        && let Err(e) = std::fs::create_dir_all(d)
    {
        eprintln!("xarast-cli: {}: {e}", d.display());
        return Exit::Render;
    }
    let registry = Registry::with_builtin();
    let ext = a.request.options.format().extension();
    let mut used = std::collections::HashSet::new();
    let mut worst = Exit::Ok;
    let (mut done, mut failed) = (0usize, 0usize);
    let (mut open_ms, mut scene_ms, mut render_ms, mut encode_ms) = (0.0, 0.0, 0.0, 0.0);
    let mut bytes = 0u64;
    for (input, open) in &files {
        let mut req = a.request.clone();
        req.destination = match (&a.output, &a.out_dir) {
            (Some(o), _) => o.clone(),
            (None, Some(d)) => output_for(input, d, ext, &mut used),
            (None, None) => unreachable!("parse requires an output"),
        };
        let t0 = Instant::now();
        let session = match open(input) {
            Ok(s) => s,
            Err(e) => {
                failed += 1;
                worst = worst.max(Exit::Import);
                eprintln!("FAILED: {}: {e}", input.display());
                continue;
            }
        };
        let t_open = ms(t0.elapsed());
        match registry.export(&session, &req, &NoProgress) {
            Ok(r) => {
                done += 1;
                open_ms += t_open;
                scene_ms += ms(r.scene_time);
                render_ms += ms(r.render_time);
                encode_ms += ms(r.encode_time);
                bytes += r.bytes_written;
                if !a.quiet {
                    let vector = matches!(req.options.format(), FormatId::Pdf | FormatId::Svg);
                    let size = if vector {
                        format!("{}x{} pt page", r.pixels.0, r.pixels.1)
                    } else {
                        format!("{}x{} px at {:.1} dpi", r.pixels.0, r.pixels.1, r.dpi)
                    };
                    println!(
                        "{} -> {}: {size}, {} cmds, {} bytes, open {:.1} ms, \
                         scene {:.1} ms, render {:.1} ms, encode {:.1} ms",
                        input.display(),
                        req.destination.display(),
                        r.commands,
                        r.bytes_written,
                        t_open,
                        ms(r.scene_time),
                        ms(r.render_time),
                        ms(r.encode_time),
                    );
                    for c in &r.compromises {
                        println!("    note: {c}");
                    }
                }
            }
            Err(e) => {
                failed += 1;
                worst = worst.max(match e {
                    ExportError::Area(_)
                    | ExportError::Sizing(_)
                    | ExportError::BadOptions { .. }
                    | ExportError::FeatureNotBuilt(_)
                    | ExportError::UnsupportedFormat { .. } => Exit::Usage,
                    _ => Exit::Render,
                });
                eprintln!("FAILED: {}: {e}", input.display());
            }
        }
    }
    if files.len() > 1 {
        println!(
            "{} files: {done} exported, {failed} failed, {bytes} bytes; open {open_ms:.1} ms, \
             scene {scene_ms:.1} ms, render {render_ms:.1} ms, encode {encode_ms:.1} ms",
            files.len(),
        );
    }
    worst
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| (*x).to_owned()).collect()
    }

    #[test]
    fn the_format_comes_from_the_flag_or_the_extension() {
        let a = parse(&argv(&["a.xar", "-o", "x.JPG"])).unwrap();
        assert_eq!(a.request.options.format(), FormatId::Jpeg);
        let a = parse(&argv(&["d", "--out-dir", "o", "--format", "webp"])).unwrap();
        assert_eq!(a.request.options.format(), FormatId::WebP);
        assert!(parse(&argv(&["d", "--out-dir", "o"])).is_err());
        assert!(parse(&argv(&["a.xar", "-o", "x.tiff"])).is_err());
    }

    #[test]
    fn xar_is_refused_with_the_architecture_reason() {
        for args in [
            &["a.xar", "-o", "b.xar"][..],
            &["a.xar", "--format", "xar", "-o", "b.png"],
            &["a.xar", "--format", "png", "-o", "b.xar"],
        ] {
            let e = parse(&argv(args)).unwrap_err();
            assert!(e.contains("§3.5"), "{e}");
        }
    }

    #[test]
    fn sizing_flags() {
        let a = parse(&argv(&["a.xar", "-o", "x.png", "--dpi", "300"])).unwrap();
        assert_eq!(a.request.sizing, ExportSizing::at_dpi(300.0));
        let a = parse(&argv(&["a.xar", "-o", "x.png", "--width=640"])).unwrap();
        assert_eq!(a.request.sizing, ExportSizing::with_pixels(640, 0));
        assert!(
            parse(&argv(&[
                "a.xar", "-o", "x.png", "--dpi", "300", "--width", "5"
            ]))
            .is_err()
        );
        assert!(parse(&argv(&["a.xar", "-o", "x.png", "--width", "0"])).is_err());
    }

    #[test]
    fn areas_and_backgrounds() {
        assert_eq!(parse_area("page"), Ok(ExportArea::Page(None)));
        assert_eq!(
            parse_area("0,0,72,36"),
            Ok(ExportArea::Rect(Rect::raw(0, 0, 72_000, 36_000)))
        );
        assert!(parse_area("1,2,3").is_err());
        assert!(parse_area("0,0,0,5").is_err());
        assert_eq!(parse_background("paper"), Ok(Background::Paper));
        assert_eq!(
            parse_background("#ff800080"),
            Ok(Background::Colour(Rgba8 {
                r: 255,
                g: 128,
                b: 0,
                a: 128
            }))
        );
        assert!(parse_background("12345").is_err());
    }

    #[test]
    fn format_options_go_to_the_right_format() {
        let a = parse(&argv(&[
            "a.xar",
            "-o",
            "x.jpg",
            "--quality",
            "70",
            "--progressive",
            "--subsampling",
            "444",
        ]))
        .unwrap();
        let FormatOptions::Jpeg(j) = a.request.options else {
            panic!("jpeg");
        };
        assert_eq!(
            (j.quality, j.progressive, j.subsampling),
            (70, true, Subsampling::S444)
        );
        assert!(parse(&argv(&["a.xar", "-o", "x.jpg", "--interlace"])).is_err());
        assert!(parse(&argv(&["a.xar", "-o", "x.png", "--progressive"])).is_err());
        let a = parse(&argv(&[
            "a.xar",
            "-o",
            "x.png",
            "--colour",
            "palette:16",
            "--depth",
            "16",
        ]))
        .unwrap();
        let FormatOptions::Png(p) = a.request.options else {
            panic!("png");
        };
        assert_eq!(p.colour, PngColour::Palette { max_colours: 16 });
        assert_eq!(p.bit_depth, PngDepth::Sixteen);
    }

    #[test]
    fn text_output_is_a_pdf_and_svg_option() {
        for (out, want) in [
            ("x.pdf", "outlines"),
            ("x.svg", "outlines"),
            ("x.svg", "text"),
        ] {
            let a = parse(&argv(&["a.xar", "-o", out, "--text", want])).unwrap();
            let got = match a.request.options {
                FormatOptions::Pdf(p) => p.text,
                FormatOptions::Svg(s) => s.text,
                _ => panic!("pdf or svg"),
            };
            let want = if want == "text" {
                TextOutput::Text
            } else {
                TextOutput::Outlines
            };
            assert_eq!(got, want);
        }
        let a = parse(&argv(&["a.xar", "-o", "x.pdf"])).unwrap();
        let FormatOptions::Pdf(p) = a.request.options else {
            panic!("pdf");
        };
        assert_eq!(p.text, TextOutput::Text, "text is the default");
        assert!(parse(&argv(&["a.xar", "-o", "x.png", "--text", "outlines"])).is_err());
        assert!(parse(&argv(&["a.xar", "-o", "x.pdf", "--text", "curves"])).is_err());
    }
}
