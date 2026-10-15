//! PDF export (phase 11 W11.4).
//!
//! One page per export, PDF 1.7, vector wherever PDF can say what the
//! renderer draws. The scene is resolved into a display list whose
//! "device" is the page in points (y up), and each command walks down the
//! **fidelity ladder**:
//!
//! 1. **Native.** Paths with non-zero and even-odd fills, strokes with
//!    caps, joins, mitre limits and dashes, clips, flat colours, constant
//!    opacity, linear, radial and diamond gradients with the ramp baked
//!    into a sampled function, four-colour meshes, transparency groups.
//! 2. **Workaround, still vector.** Conical gradients as a Gouraud fan,
//!    three-colour meshes as a sampled grid, strokes with different start
//!    and end caps (or gradient strokes) as their outline. Reported as
//!    [`Compromise::Approximated`] where not exact.
//! 3. **Rasterise that object** through the CPU backend at
//!    [`PdfOptions::rasterise_dpi`]: perspective gradients, bitmap fills
//!    and placed images (until T11.4.8), graduated transparency (until
//!    T11.4.5), the Positive/Negative fill rules, and every transparency
//!    family other than ordinary mixing (with its backdrop; see
//!    [`BlendFidelity`]). Each is a [`Compromise::Rasterised`] naming the
//!    object.
//!
//! The crate behind it is `pdf-writer`, reached only through
//! [`writer`]; the T11.4.1 spike that chose it is in
//! `docs/memory/export.md`.
//!
//! Text is drawn **as text** when the source says which outlines are text
//! (`SourceScene::text`, T11.4.7): each face used is embedded once as a
//! subset `CIDFont` with a `ToUnicode` map, so the text is selectable and
//! searchable. A run painted with a flat colour or a gradient is shown in
//! that font (the gradient through a text clip); anything else (bitmap
//! fills, rasterised transparency, variable-font instances, an underline
//! under non-opaque paint) is drawn as before — outlines or pixels — with
//! the same glyphs as invisible text on top. A face whose `fsType` forbids
//! embedding is never embedded: its text stays outlines and the report
//! carries `FontNotEmbedded` (`pdf/text.rs`).

pub mod rasterise;
pub mod shading;
mod text;
pub mod writer;

use std::sync::Arc;
use std::time::Instant;

use kurbo::{Affine, BezPath, Shape as _};
use xarast_geom::{Cap as GCap, FillRule, Join as GJoin, Mp, Rect, StrokeStyle};
use xarast_render::display_list::DrawItem;
use xarast_render::{
    BlendFamily, DeviceRect, DirtyRect, DisplayList, DrawCmd, LayerKind, Paint, SceneNodeId,
    Transform2D, TranspSource, Transparency, ViewParams,
};

use crate::model::{Background, ExportRequest};
use crate::options::{
    BlendFidelity, FormatId, FormatOptions, PDF_RASTERISE_DPI, PdfOptions, TextOutput,
};
use crate::raster::AtomicFile;
use crate::registry::{Capabilities, Exporter};
use crate::report::{Compromise, ExportError, ExportReport};
use crate::source::{ExportSource, Progress, SourceScene, Stage};

use rasterise::{Rasteriser, Target};
use shading::{Shaded, paint_gradient, paint_opacity};
use text::PdfText;
use writer::{
    Blend, Canvas, Cap, DocInfo, GState, Join, LineStyle, PageBoxes, PdfWriter, Rule, TextMode,
};

/// The largest page side, in points: PDF 1.7's implementation limit
/// (Annex C), 200 inches. A larger export is refused rather than written
/// as a file viewers clip.
pub const MAX_PAGE_POINTS: f64 = 14_400.0;

/// The page an export makes: its size in points and the map from the
/// document onto it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PagePlan {
    /// The document rectangle, bleed included.
    pub area: Rect,
    /// Page width in points.
    pub width: f64,
    /// Page height in points.
    pub height: f64,
    /// The bleed on every side, in points.
    pub bleed: f64,
    /// Document millipoints to page points, y up.
    pub to_page: Transform2D,
}

/// Resolves the area and the page size. The page is the requested
/// physical size, or the area's own size when none is given; resolution
/// and pixel counts mean nothing to a vector page.
///
/// # Errors
///
/// [`ExportError::Area`], [`ExportError::Sizing`], or `BadOptions` for a
/// page over [`MAX_PAGE_POINTS`].
pub fn plan_page(src: &dyn ExportSource, req: &ExportRequest) -> Result<PagePlan, ExportError> {
    let area = req.bled(src.resolve_area(&req.area)?)?;
    let (aw, ah) = (area.width().to_f64(), area.height().to_f64());
    let mut sizing = req.sizing;
    let (pw, ph) = match sizing.resolve(area) {
        Ok(()) => (sizing.physical.0.to_f64(), sizing.physical.1.to_f64()),
        // Only the pixel count is out of range: a vector page does not
        // care, and falls back to the area's own size.
        Err(crate::SizingError::TooLarge { .. }) => (aw, ah),
        Err(e) => return Err(e.into()),
    };
    let per_pt = f64::from(Mp::PER_PT);
    let (width, height) = (pw / per_pt, ph / per_pt);
    if !(width > 0.0 && height > 0.0) || width > MAX_PAGE_POINTS || height > MAX_PAGE_POINTS {
        return Err(ExportError::BadOptions {
            format: FormatId::Pdf,
            reason: format!(
                "a {width:.1} x {height:.1} pt page is outside PDF's {MAX_PAGE_POINTS} pt limit"
            ),
        });
    }
    let (sx, sy) = (width / aw, height / ah);
    let (x0, y0) = (area.lo.x.to_f64(), area.lo.y.to_f64());
    Ok(PagePlan {
        area,
        width,
        height,
        bleed: req.bleed.to_f64().max(0.0) * sx,
        to_page: Transform2D::new([sx, 0.0, 0.0, sy, -x0 * sx, -y0 * sy]),
    })
}

/// PDF 1.7.
#[derive(Debug, Clone, Copy, Default)]
pub struct PdfExporter;

impl PdfExporter {
    fn options(req: &ExportRequest) -> Result<PdfOptions, ExportError> {
        let FormatOptions::Pdf(o) = req.options else {
            return Err(ExportError::BadOptions {
                format: FormatId::Pdf,
                reason: "options are not PDF options".into(),
            });
        };
        if !(PDF_RASTERISE_DPI.0..=PDF_RASTERISE_DPI.1).contains(&o.rasterise_dpi) {
            return Err(ExportError::BadOptions {
                format: FormatId::Pdf,
                reason: format!(
                    "a rasterising resolution of {} dpi is outside {}..={}",
                    o.rasterise_dpi, PDF_RASTERISE_DPI.0, PDF_RASTERISE_DPI.1
                ),
            });
        }
        Ok(o)
    }
}

impl Exporter for PdfExporter {
    fn id(&self) -> FormatId {
        FormatId::Pdf
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["pdf"]
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            vector: true,
            alpha: true,
            // One page per export until page selection lands (T11.4.9).
            multipage: false,
            // Subset fonts with ToUnicode maps (T11.4.7), where the face's
            // licence allows.
            embeds_fonts: true,
            has_dpi: false,
            lossy: false,
            deterministic: true,
            max_side: xarast_render::export::MAX_EXPORT_SIDE,
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
        let page = plan_page(src, req)?;
        let mut report = ExportReport::default();
        progress.report(Stage::Scene, 0.0);
        let t = Instant::now();
        let built = src.build_scene(req.quality)?;
        report.scene_time = t.elapsed();
        progress.report(Stage::Scene, 1.0);
        if progress.cancelled() {
            return Err(ExportError::Cancelled);
        }
        let t = Instant::now();
        let paper = src.paper_colour();
        let bytes = write_pdf(&built, &page, req, &o, paper, progress, &mut report)?;
        report.render_time = t.elapsed();
        progress.report(Stage::Encode, 1.0);
        let te = Instant::now();
        report.bytes_written = AtomicFile::new(&req.destination).write_all(&bytes)?;
        report.encode_time = te.elapsed();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        {
            report.pixels = (page.width.round() as u32, page.height.round() as u32);
        }
        report.dpi = 72.0;
        report.compromises.extend(built.compromises);
        report
            .compromises
            .extend(crate::fidelity::document_compromises(
                src,
                crate::fidelity::Target::Pdf,
            ));
        report.duration = t0.elapsed();
        Ok(report)
    }
}

/// Builds the whole file in memory.
///
/// # Errors
///
/// `Cancelled`, or `Render` when a rasterised object fails.
pub fn write_pdf(
    built: &SourceScene,
    page: &PagePlan,
    req: &ExportRequest,
    o: &PdfOptions,
    paper: xarast_color::Rgba8,
    progress: &dyn Progress,
    report: &mut ExportReport,
) -> Result<Vec<u8>, ExportError> {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let viewport = DeviceRect::from_size(page.width.ceil() as u32, page.height.ceil() as u32);
    let view = ViewParams {
        transform: page.to_page,
        viewport,
        quality: req.quality,
        dpi: 72.0,
    };
    let dl = DisplayList::build(&built.scene, &view, &DirtyRect::of(viewport));
    report.commands = dl.len();
    let page_rect = kurbo::Rect::new(0.0, 0.0, page.width, page.height);
    // What a blend rendered with its backdrop reads under everything: the
    // page as the user saw it in the editor, so always opaque. Over a
    // transparent page that is the paper; the rectangle such an object
    // covers is therefore opaque in the file. (Rendering over nothing
    // would not help: a blend reads a colour, and the renderer mixes
    // towards black over a transparent destination.)
    let clear = match req.background {
        Background::Transparent => Background::Paper.clear_colour(paper, false),
        bg => bg.clear_colour(paper, false),
    };
    let mut w = PdfWriter::new(o.compress);
    let mut compromises = Vec::new();
    let text = built
        .text
        .as_ref()
        .filter(|_| o.text == TextOutput::Text)
        .map(|t| PdfText::new(t, &mut w, &mut compromises));
    let mut tx = Translator {
        w,
        dl: &dl,
        built,
        o: *o,
        canvases: vec![Canvas::new()],
        frames: Vec::new(),
        in_effect: 0,
        compromises,
        text,
        raster: Rasteriser::new(
            &built.scene,
            &built.resolver,
            page.to_page,
            page_rect,
            f64::from(o.rasterise_dpi),
            req.quality,
            clear,
        ),
        page: page_rect,
        transparent_page: req.background == Background::Transparent,
    };
    tx.background(req.background, paper);
    let cmds = dl.commands();
    let cancelled = || progress.cancelled();
    for (i, cmd) in cmds.iter().enumerate() {
        if i % 256 == 0 {
            if progress.cancelled() {
                return Err(ExportError::Cancelled);
            }
            #[allow(clippy::cast_precision_loss)]
            progress.report(Stage::Render, i as f32 / cmds.len().max(1) as f32);
        }
        tx.command(cmd, &cancelled)?;
    }
    let Translator {
        mut w,
        mut canvases,
        compromises,
        ..
    } = tx;
    // Unbalanced layers: fold every open one into its parent, opaquely.
    while canvases.len() > 1 {
        let inner = canvases.pop().unwrap_or_default();
        let g = w.group(inner, [0.0, 0.0, page.width, page.height], false);
        if let Some(top) = canvases.last_mut() {
            top.xobject(g);
        }
    }
    let content = canvases.pop().unwrap_or_default();
    let trim = (page.bleed > 0.0).then(|| {
        let b = page.bleed;
        [b, b, page.width - b, page.height - b]
    });
    w.page(
        content,
        PageBoxes {
            media: [0.0, 0.0, page.width, page.height],
            trim,
        },
    );
    report.compromises.extend(compromises);
    Ok(w.finish(&DocInfo {
        title: None,
        producer: format!("Xarast {}", env!("CARGO_PKG_VERSION")),
    }))
}

/// What the translator has open.
#[derive(Debug)]
enum Frame {
    /// A clip: one `q` on the current canvas.
    Clip,
    /// A layer: its own canvas, composited when it closes.
    Layer {
        kind: LayerKind,
        /// The page area its content touches.
        bounds: DeviceRect,
    },
}

/// How a transparency family is drawn.
enum BlendPlan {
    /// With this blend mode; the family's name when it only approximates.
    Native(Blend, Option<&'static str>),
    /// Not at all: the None family.
    Skip,
    /// Rasterised with its backdrop.
    Backdrop(String),
}

fn blend_plan(family: BlendFamily, fidelity: BlendFidelity) -> BlendPlan {
    match (family, fidelity) {
        (BlendFamily::Mix, _) => BlendPlan::Native(Blend::Normal, None),
        (BlendFamily::None, _) => BlendPlan::Skip,
        (BlendFamily::StainedGlass, BlendFidelity::PreferNative) => {
            BlendPlan::Native(Blend::Multiply, Some("Multiply"))
        }
        (BlendFamily::Bleach, BlendFidelity::PreferNative) => {
            BlendPlan::Native(Blend::Screen, Some("Screen"))
        }
        (f, _) => BlendPlan::Backdrop(format!("{f:?} transparency has no exact PDF blend mode")),
    }
}

/// Opacity from a paint alpha and a flat Xara transparency level (0
/// opaque), in 1 / [`OPACITY_ONE`].
fn opacity(alpha: u8, t: u8) -> u32 {
    u32::from(alpha) * (255 - u32::from(t))
}

struct Translator<'a> {
    w: PdfWriter,
    dl: &'a DisplayList,
    /// How deep inside a live effect the stream is: what it wraps was
    /// rasterised with it, at its push, so it is skipped.
    in_effect: usize,
    built: &'a SourceScene,
    o: PdfOptions,
    canvases: Vec<Canvas>,
    frames: Vec<Frame>,
    compromises: Vec<Compromise>,
    /// The text behind the scene's outlines, when the source gave it.
    text: Option<PdfText>,
    raster: Rasteriser<'a>,
    page: kurbo::Rect,
    /// The page has no background, so backdrops are rendered on the paper.
    transparent_page: bool,
}

impl Translator<'_> {
    fn canvas(&mut self) -> &mut Canvas {
        if self.canvases.is_empty() {
            self.canvases.push(Canvas::new());
        }
        let n = self.canvases.len() - 1;
        &mut self.canvases[n]
    }

    fn background(&mut self, bg: Background, paper: xarast_color::Rgba8) {
        let c = match bg {
            Background::Transparent => return,
            Background::Paper => paper,
            Background::Colour(c) => c,
        };
        if c.a == 0 {
            return;
        }
        let g = (c.a < 255).then(|| {
            self.w.gstate(GState {
                fill_alpha: u32::from(c.a) * 255,
                ..GState::DEFAULT
            })
        });
        let page = self.page;
        let cv = self.canvas();
        cv.save();
        if let Some(g) = g {
            cv.gstate(g);
        }
        cv.fill_rgb([c.r, c.g, c.b]);
        cv.rect(page.x0, page.y0, page.x1, page.y1);
        cv.fill(Rule::NonZero);
        cv.restore();
    }

    /// Adds `b` to every open layer's bounds.
    fn touch(&mut self, b: DeviceRect) {
        for f in &mut self.frames {
            if let Frame::Layer { bounds, .. } = f {
                *bounds = bounds.union(b);
            }
        }
    }

    fn command(
        &mut self,
        cmd: &DrawCmd,
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Result<(), ExportError> {
        let dl = self.dl;
        if self.in_effect > 0 {
            match cmd {
                DrawCmd::PushEffect { .. } => self.in_effect += 1,
                DrawCmd::PopEffect => self.in_effect -= 1,
                _ => {}
            }
            return Ok(());
        }
        match (dl.item(cmd), *cmd) {
            // A live effect is pixels the renderer computes (a feather's
            // blurred mask): PDF has nothing that draws it, so it is
            // rasterised whole, alone, and what it wraps is skipped.
            // A shadow draws beyond what it wraps: the image covers its
            // growth too.
            (
                DrawItem::PushEffect {
                    effect,
                    xf,
                    content,
                },
                DrawCmd::PushEffect { op, .. },
            ) => {
                self.in_effect = 1;
                let area = content.inflated(effect.growth_px(xf.max_scale()));
                self.touch(area);
                let reason = match effect {
                    xarast_render::LayerEffect::Feather { .. } => {
                        "a live effect (feather) has no PDF equivalent"
                    }
                    xarast_render::LayerEffect::Shadow(_) => {
                        "a live effect (shadow) has no PDF equivalent"
                    }
                    xarast_render::LayerEffect::Bevel(_) => {
                        "a live effect (bevel) has no PDF equivalent"
                    }
                };
                self.rasterise(
                    SceneNodeId(0),
                    Target::Effect(op),
                    area,
                    false,
                    reason,
                    cancelled,
                )?;
            }
            (DrawItem::PushClip { path, rule, xf }, _) => {
                let p = xf.to_affine() * path.bez().clone();
                let rule = match rule {
                    FillRule::EvenOdd => Rule::EvenOdd,
                    FillRule::NonZero => Rule::NonZero,
                    other => {
                        self.compromises.push(Compromise::Approximated {
                            node: SceneNodeId(0),
                            what: format!("a clip with the {other:?} winding rule drawn non-zero")
                                .into(),
                        });
                        Rule::NonZero
                    }
                };
                let cv = self.canvas();
                cv.save();
                cv.path(&p);
                cv.clip(rule);
                self.frames.push(Frame::Clip);
            }
            (DrawItem::PopClip, _) => {
                if matches!(self.frames.last(), Some(Frame::Clip)) {
                    self.frames.pop();
                    self.canvas().restore();
                }
            }
            (DrawItem::PushLayer { kind, .. }, _) => {
                self.canvases.push(Canvas::new());
                self.frames.push(Frame::Layer {
                    kind,
                    bounds: DeviceRect::EMPTY,
                });
            }
            (DrawItem::PopLayer { opacity: t, .. }, DrawCmd::PopLayer { layer, .. }) => {
                self.pop_layer(t, layer, cancelled)?;
            }
            (
                DrawItem::Fill {
                    node,
                    path,
                    rule,
                    paint,
                    xf,
                    transparency,
                    bounds,
                },
                _,
            ) => {
                self.touch(bounds);
                let op = DisplayList::op_of(cmd).unwrap_or(u32::MAX);
                let run = self.text.as_ref().and_then(|t| t.run_of(path));
                match run {
                    Some(run) => self.fill_text(
                        run,
                        node,
                        op,
                        path.bez(),
                        paint,
                        xf,
                        transparency,
                        bounds,
                        cancelled,
                    )?,
                    None => self.fill(
                        node,
                        op,
                        path.bez(),
                        rule,
                        paint,
                        xf,
                        transparency,
                        bounds,
                        cancelled,
                    )?,
                }
            }
            (
                DrawItem::Stroke {
                    node,
                    path,
                    style,
                    paint,
                    xf,
                    transparency,
                    bounds,
                },
                _,
            ) => {
                self.touch(bounds);
                let op = DisplayList::op_of(cmd).unwrap_or(u32::MAX);
                self.stroke(
                    node,
                    op,
                    path.bez(),
                    style,
                    paint,
                    xf,
                    transparency,
                    bounds,
                    cancelled,
                )?;
                // Stroked-only text: still selectable, as invisible text.
                if let Some(run) = self.text.as_ref().and_then(|t| t.run_of(path)) {
                    self.invisible_text(run, xf);
                }
            }
            (
                DrawItem::Image {
                    node,
                    transparency,
                    bounds,
                    ..
                },
                _,
            ) => {
                self.touch(bounds);
                let op = DisplayList::op_of(cmd).unwrap_or(u32::MAX);
                let backdrop = match blend_plan(transparency.family, self.o.blend_fidelity) {
                    BlendPlan::Skip => return Ok(()),
                    BlendPlan::Native(Blend::Normal, _) => false,
                    _ => true,
                };
                self.rasterise(
                    node,
                    Target::Op(op),
                    bounds,
                    backdrop,
                    "a placed image (PDF image embedding is T11.4.8)",
                    cancelled,
                )?;
            }
            _ => {}
        }
        Ok(())
    }

    fn pop_layer(
        &mut self,
        t: &Transparency,
        layer: u32,
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Result<(), ExportError> {
        // Close clips opened inside the layer and never closed.
        while matches!(self.frames.last(), Some(Frame::Clip)) {
            self.frames.pop();
            self.canvas().restore();
        }
        let Some(Frame::Layer { kind, bounds }) = self.frames.pop() else {
            return Ok(());
        };
        if self.canvases.len() < 2 {
            return Ok(());
        }
        let inner = self.canvases.pop().unwrap_or_default();
        let page = self.page;
        let plan = blend_plan(t.family, self.o.blend_fidelity);
        let level = match (&t.source, &plan) {
            (_, BlendPlan::Skip) => return Ok(()),
            (TranspSource::Flat(l), BlendPlan::Native(..)) => Some(*l),
            _ => None,
        };
        match (level, plan) {
            (Some(l), BlendPlan::Native(blend, approx)) => {
                if let Some(theirs) = approx {
                    self.compromises.push(Compromise::BlendModeApproximated {
                        node: SceneNodeId(0),
                        ours: t.family,
                        theirs: theirs.into(),
                    });
                }
                let isolated = !matches!(kind, LayerKind::Plain);
                let g = self
                    .w
                    .group(inner, [page.x0, page.y0, page.x1, page.y1], isolated);
                let gs = GState {
                    fill_alpha: opacity(255, l),
                    stroke_alpha: opacity(255, l),
                    blend,
                    mask: None,
                };
                let gs = (!gs.is_default()).then(|| self.w.gstate(gs));
                let cv = self.canvas();
                cv.save();
                if let Some(gs) = gs {
                    cv.gstate(gs);
                }
                cv.xobject(g);
                cv.restore();
            }
            _ => {
                drop(inner);
                let reason = match t.source {
                    TranspSource::Flat(_) => format!(
                        "a layer with {:?} transparency has no exact PDF blend mode",
                        t.family
                    ),
                    _ => "a layer with graduated or bitmap transparency (T11.4.5)".to_owned(),
                };
                self.rasterise(
                    SceneNodeId(0),
                    Target::Layer(layer),
                    bounds,
                    true,
                    &reason,
                    cancelled,
                )?;
            }
        }
        Ok(())
    }

    fn rasterise(
        &mut self,
        node: SceneNodeId,
        target: Target,
        bounds: DeviceRect,
        backdrop: bool,
        reason: &str,
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Result<(), ExportError> {
        let Some(r) = self.raster.render(bounds, target, backdrop, cancelled)? else {
            return Ok(());
        };
        let img = self.w.image(r.width, r.height, &r.rgb, r.alpha.as_deref());
        let [x0, y0, x1, y1] = r.rect;
        let cv = self.canvas();
        cv.save();
        cv.transform([x1 - x0, 0.0, 0.0, y1 - y0, x0, y0]);
        cv.xobject(img);
        cv.restore();
        let reason = if backdrop {
            let on = if self.transparent_page {
                " over the paper"
            } else {
                ""
            };
            format!("{reason} (rendered with its backdrop{on})")
        } else {
            reason.to_owned()
        };
        self.compromises.push(Compromise::Rasterised {
            node,
            reason: Arc::from(reason),
            dpi: r.dpi,
        });
        Ok(())
    }

    /// The graphics state for a native draw, or why the object must be
    /// rasterised (`Err(backdrop, reason)`), or `Ok(None)` to draw nothing.
    #[allow(clippy::type_complexity)]
    fn state_for(
        &mut self,
        node: SceneNodeId,
        paint_alpha: u8,
        t: &Transparency,
        bounds: DeviceRect,
    ) -> Result<Option<Option<writer::Resource>>, (bool, String)> {
        let plan = blend_plan(t.family, self.o.blend_fidelity);
        let (blend, approx) = match plan {
            BlendPlan::Skip => return Ok(None),
            BlendPlan::Backdrop(reason) => return Err((true, reason)),
            BlendPlan::Native(b, a) => (b, a),
        };
        let (a, mask) = match &t.source {
            TranspSource::Flat(level) => (opacity(paint_alpha, *level), None),
            TranspSource::Gradient {
                shape,
                mapping,
                repeat,
                ramp,
            } => {
                // Graduated transparency: a luminosity soft mask painting
                // the opacity ramp with the same shading machinery as a
                // colour gradient (T11.4.5).
                let levels = self
                    .built
                    .resolver
                    .transparency_ramps
                    .get(ramp.index() as usize)
                    .map_or(&[][..], Vec::as_slice);
                let bbox = kurbo::Rect::new(
                    f64::from(bounds.x0),
                    f64::from(bounds.y0),
                    f64::from(bounds.x1),
                    f64::from(bounds.y1),
                )
                .intersect(self.page);
                let mut m = Canvas::new();
                m.save();
                let out =
                    paint_opacity(&mut self.w, &mut m, *shape, *mapping, *repeat, levels, bbox);
                m.restore();
                match out {
                    Shaded::Rasterise(reason) => return Err((blend != Blend::Normal, reason)),
                    Shaded::Approximated(what) => self.compromises.push(Compromise::Approximated {
                        node,
                        what: format!("its graduated transparency: {what}").into(),
                    }),
                    Shaded::Exact => {}
                }
                let page = self.page;
                let mask = self.w.soft_mask(m, [page.x0, page.y0, page.x1, page.y1]);
                (u32::from(paint_alpha) * 255, Some(mask))
            }
            TranspSource::Image { .. } => {
                return Err((
                    blend != Blend::Normal,
                    "bitmap transparency (T11.4.5, T11.4.8)".into(),
                ));
            }
            TranspSource::Mesh { .. } => {
                return Err((
                    blend != Blend::Normal,
                    "three- or four-colour transparency".into(),
                ));
            }
        };
        if a == 0 && blend == Blend::Normal {
            return Ok(None);
        }
        if let Some(theirs) = approx {
            self.compromises.push(Compromise::BlendModeApproximated {
                node,
                ours: t.family,
                theirs: theirs.into(),
            });
        }
        let g = GState {
            fill_alpha: a,
            stroke_alpha: a,
            blend,
            mask,
        };
        Ok(Some((!g.is_default()).then(|| self.w.gstate(g))))
    }

    #[allow(clippy::too_many_arguments)]
    fn fill(
        &mut self,
        node: SceneNodeId,
        op: u32,
        path: &BezPath,
        rule: FillRule,
        paint: &Paint,
        xf: Transform2D,
        t: &Transparency,
        bounds: DeviceRect,
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Result<(), ExportError> {
        if path.elements().is_empty() {
            return Ok(());
        }
        let pdf_rule = match rule {
            FillRule::NonZero => Rule::NonZero,
            FillRule::EvenOdd => Rule::EvenOdd,
            other => {
                let backdrop = t.family != BlendFamily::Mix;
                return self.rasterise(
                    node,
                    Target::Op(op),
                    bounds,
                    backdrop,
                    &format!("the {other:?} winding rule has no PDF equivalent"),
                    cancelled,
                );
            }
        };
        let page_path = xf.to_affine() * path.clone();
        self.paint_area(
            node, op, &page_path, pdf_rule, paint, t, bounds, None, cancelled,
        )
        .map(|_| ())
    }

    /// A text run's fill: shown in its embedded font when the plan and the
    /// paint allow, otherwise filled as outlines (or pixels) with the
    /// glyphs as invisible text over them.
    #[allow(clippy::too_many_arguments)]
    fn fill_text(
        &mut self,
        run: usize,
        node: SceneNodeId,
        op: u32,
        path: &BezPath,
        paint: &Paint,
        xf: Transform2D,
        t: &Transparency,
        bounds: DeviceRect,
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Result<(), ExportError> {
        let Some(plan) = self.text.as_ref().and_then(|x| x.plans.get(run)) else {
            return self.fill(
                node,
                op,
                path,
                FillRule::NonZero,
                paint,
                xf,
                t,
                bounds,
                cancelled,
            );
        };
        let paintable = matches!(paint, Paint::Solid(_) | Paint::Gradient { .. });
        // Glyphs and underline are painted apart: fine while the paint is
        // opaque and plain, a double coat where they overlap otherwise.
        let plain = match paint {
            Paint::Solid(c) => c.a == 255,
            _ => true,
        } && t.family == BlendFamily::Mix
            && matches!(t.source, TranspSource::Flat(0));
        if !(plan.native && paintable && (plan.decoration.is_none() || plain)) {
            self.fill(
                node,
                op,
                path,
                FillRule::NonZero,
                paint,
                xf,
                t,
                bounds,
                cancelled,
            )?;
            self.invisible_text(run, xf);
            return Ok(());
        }
        let m = xf.to_affine();
        let area = TextArea {
            glyphs: plan
                .glyphs
                .iter()
                .map(|g| (g.font, g.cid, (m * g.em).as_coeffs()))
                .collect(),
            decoration: plan.decoration.as_ref().map(|d| m * (**d).clone()),
        };
        let page_path = m * path.clone();
        let drawn = self.paint_area(
            node,
            op,
            &page_path,
            Rule::NonZero,
            paint,
            t,
            bounds,
            Some(&area),
            cancelled,
        )?;
        match drawn {
            Painted::AsText => {
                if let Some(x) = self.text.as_mut()
                    && let Some(d) = x.done.get_mut(run)
                {
                    *d = true;
                }
            }
            Painted::Otherwise => self.invisible_text(run, xf),
            Painted::Nothing => {}
        }
        Ok(())
    }

    /// The run's glyphs as invisible text (rendering mode 3), once per
    /// run: what makes text drawn as outlines or pixels selectable.
    fn invisible_text(&mut self, run: usize, xf: Transform2D) {
        let Some(x) = self.text.as_mut() else {
            return;
        };
        if x.done.get(run).copied().unwrap_or(true) {
            return;
        }
        x.done[run] = true;
        let m = xf.to_affine();
        let glyphs: Vec<_> = x.plans[run]
            .glyphs
            .iter()
            .map(|g| (g.font, g.cid, (m * g.em).as_coeffs()))
            .collect();
        if glyphs.is_empty() {
            return;
        }
        let cv = self.canvas();
        cv.begin_text(TextMode::Invisible);
        for (font, cid, em) in glyphs {
            cv.glyph(font, em, cid);
        }
        cv.end_text();
    }

    /// Paints `paint` over `page_path` (page space) with `rule`, or, with
    /// `text`, over its glyphs and decoration (`page_path` then only
    /// bounds the paint).
    #[allow(clippy::too_many_arguments)]
    fn paint_area(
        &mut self,
        node: SceneNodeId,
        op: u32,
        page_path: &BezPath,
        rule: Rule,
        paint: &Paint,
        t: &Transparency,
        bounds: DeviceRect,
        text: Option<&TextArea>,
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Result<Painted, ExportError> {
        let alpha = match paint {
            Paint::Solid(c) => c.a,
            _ => 255,
        };
        let gs = match self.state_for(node, alpha, t, bounds) {
            Ok(None) => return Ok(Painted::Nothing),
            Ok(Some(gs)) => gs,
            Err((backdrop, reason)) => {
                self.rasterise(node, Target::Op(op), bounds, backdrop, &reason, cancelled)?;
                return Ok(Painted::Otherwise);
            }
        };
        match paint {
            Paint::Solid(c) => {
                let cv = self.canvas();
                if let Some(g) = gs {
                    cv.save();
                    cv.gstate(g);
                }
                cv.fill_rgb([c.r, c.g, c.b]);
                match text {
                    Some(a) => {
                        a.show(cv, TextMode::Fill);
                        if let Some(d) = &a.decoration {
                            cv.path(d);
                            cv.fill(Rule::NonZero);
                        }
                    }
                    None => {
                        cv.path(page_path);
                        cv.fill(rule);
                    }
                }
                if gs.is_some() {
                    cv.restore();
                }
                Ok(if text.is_some() {
                    Painted::AsText
                } else {
                    Painted::Otherwise
                })
            }
            Paint::Gradient {
                shape,
                mapping,
                repeat,
                ramp,
            } => {
                let bbox = page_path.bounding_box();
                // Paint into a scratch canvas first: a gradient that turns
                // out to need rasterising must leave no trace. Text clips
                // with its glyphs (mode 7), then its decoration clips a
                // second coat.
                let mut scratch = Canvas::new();
                let mut out = Shaded::Exact;
                let mut coats: Vec<Option<&BezPath>> = Vec::new();
                match text {
                    Some(a) => {
                        coats.push(None);
                        if let Some(d) = &a.decoration {
                            coats.push(Some(d));
                        }
                    }
                    None => coats.push(Some(page_path)),
                }
                for coat in coats {
                    scratch.save();
                    if let Some(g) = gs {
                        scratch.gstate(g);
                    }
                    match (coat, text) {
                        (Some(p), None) => {
                            scratch.path(p);
                            scratch.clip(rule);
                        }
                        (Some(p), Some(_)) => {
                            scratch.path(p);
                            scratch.clip(Rule::NonZero);
                        }
                        (None, Some(a)) => a.show(&mut scratch, TextMode::Clip),
                        (None, None) => {}
                    }
                    let o = paint_gradient(
                        &mut self.w,
                        &mut scratch,
                        *shape,
                        *mapping,
                        *repeat,
                        ramp,
                        &self.built.resolver.ramps,
                        bbox,
                    );
                    scratch.restore();
                    match o {
                        Shaded::Rasterise(_) => {
                            out = o;
                            break;
                        }
                        Shaded::Approximated(_) => out = o,
                        Shaded::Exact => {}
                    }
                }
                match out {
                    Shaded::Rasterise(reason) => {
                        let backdrop = t.family != BlendFamily::Mix;
                        self.rasterise(node, Target::Op(op), bounds, backdrop, &reason, cancelled)?;
                        Ok(Painted::Otherwise)
                    }
                    other => {
                        if let Shaded::Approximated(what) = other {
                            self.compromises.push(Compromise::Approximated {
                                node,
                                what: what.into(),
                            });
                        }
                        self.canvas().append(scratch);
                        Ok(if text.is_some() {
                            Painted::AsText
                        } else {
                            Painted::Otherwise
                        })
                    }
                }
            }
            Paint::Image { .. } | Paint::Fractal(_) => {
                let backdrop = t.family != BlendFamily::Mix;
                self.rasterise(
                    node,
                    Target::Op(op),
                    bounds,
                    backdrop,
                    "a bitmap fill (PDF image embedding is T11.4.8)",
                    cancelled,
                )?;
                Ok(Painted::Otherwise)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn stroke(
        &mut self,
        node: SceneNodeId,
        op: u32,
        path: &BezPath,
        style: &StrokeStyle,
        paint: &Paint,
        xf: Transform2D,
        t: &Transparency,
        bounds: DeviceRect,
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Result<(), ExportError> {
        if path.elements().is_empty() || style.width.raw() < 0 {
            return Ok(());
        }
        let hairline = style.width == Mp::ZERO;
        let dash = style
            .dash
            .as_ref()
            .map(|d| (d.resolved(style.width), d.offset.to_f64()))
            .unwrap_or_default();
        let solid = matches!(paint, Paint::Solid(_));
        let same_caps = style.cap_start == style.cap_end;
        if solid && (same_caps || hairline) {
            let Paint::Solid(c) = paint else {
                return Ok(());
            };
            let gs = match self.state_for(node, c.a, t, bounds) {
                Ok(None) => return Ok(()),
                Ok(Some(gs)) => gs,
                Err((backdrop, reason)) => {
                    return self.rasterise(
                        node,
                        Target::Op(op),
                        bounds,
                        backdrop,
                        &reason,
                        cancelled,
                    );
                }
            };
            // The stroke is drawn in document units under the object's
            // transform, as the renderer strokes, so a skewed or
            // non-uniformly scaled stroke keeps its shape; coordinates are
            // taken relative to the path's corner so they stay small.
            let r = path.bounding_box();
            let local = Affine::translate((-r.x0, -r.y0)) * path.clone();
            let m = xf.to_affine() * Affine::translate((r.x0, r.y0));
            let cv = self.canvas();
            cv.save();
            if let Some(g) = gs {
                cv.gstate(g);
            }
            cv.stroke_rgb([c.r, c.g, c.b]);
            cv.transform(m.as_coeffs());
            cv.line_style(&LineStyle {
                width: if hairline { 0.0 } else { style.width.to_f64() },
                cap: cap(style.cap_start),
                join: join(style.join),
                mitre_limit: style.mitre_limit,
                dash,
            });
            cv.path(&local);
            cv.stroke();
            cv.restore();
            return Ok(());
        }
        if hairline {
            let backdrop = t.family != BlendFamily::Mix;
            return self.rasterise(
                node,
                Target::Op(op),
                bounds,
                backdrop,
                "a hairline with a gradient or bitmap paint",
                cancelled,
            );
        }
        // Different start and end caps, or a paint that is not a flat
        // colour: stroke to an outline in document space, then fill it.
        let mut k = kurbo::Stroke::new(style.width.to_f64())
            .with_start_cap(kcap(style.cap_start))
            .with_end_cap(kcap(style.cap_end))
            .with_join(kjoin(style.join))
            .with_miter_limit(style.mitre_limit.max(1.0));
        if !dash.0.is_empty() {
            k = k.with_dashes(dash.1, dash.0.iter().copied());
        }
        // A hundredth of a point, in document units.
        let tol = 10.0;
        let outline = kurbo::stroke(path.iter(), &k, &kurbo::StrokeOpts::default(), tol);
        let page_path = xf.to_affine() * outline;
        self.paint_area(
            node,
            op,
            &page_path,
            Rule::NonZero,
            paint,
            t,
            bounds,
            None,
            cancelled,
        )
        .map(|_| ())
    }
}

/// What [`Translator::paint_area`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Painted {
    /// Painted through the glyphs of an embedded font.
    AsText,
    /// Painted some other way (a path, pixels).
    Otherwise,
    /// Nothing to paint (fully transparent, or the None blend).
    Nothing,
}

/// A run's glyphs and decoration in page space.
struct TextArea {
    glyphs: Vec<(writer::Resource, u16, [f64; 6])>,
    decoration: Option<BezPath>,
}

impl TextArea {
    fn show(&self, cv: &mut Canvas, mode: TextMode) {
        cv.begin_text(mode);
        for (font, cid, m) in &self.glyphs {
            cv.glyph(*font, *m, *cid);
        }
        cv.end_text();
    }
}

fn cap(c: GCap) -> Cap {
    match c {
        GCap::Butt => Cap::Butt,
        GCap::Round => Cap::Round,
        GCap::Square => Cap::Square,
    }
}

fn join(j: GJoin) -> Join {
    match j {
        GJoin::Mitre => Join::Mitre,
        GJoin::Round => Join::Round,
        GJoin::Bevel => Join::Bevel,
    }
}

fn kcap(c: GCap) -> kurbo::Cap {
    match c {
        GCap::Butt => kurbo::Cap::Butt,
        GCap::Round => kurbo::Cap::Round,
        GCap::Square => kurbo::Cap::Square,
    }
}

fn kjoin(j: GJoin) -> kurbo::Join {
    match j {
        GJoin::Mitre => kurbo::Join::Miter,
        GJoin::Round => kurbo::Join::Round,
        GJoin::Bevel => kurbo::Join::Bevel,
    }
}
