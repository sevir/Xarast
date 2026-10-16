//! The bridge from a story in the document to glyph outlines in the scene
//! (phase 9, W9.2 attribute bridge and the walker's text path).
//!
//! [`StoryText`] (in `xarast-doc`) gives the logical text and its resolved
//! attribute runs; this module turns those into the shaper's input
//! ([`story_input`], the table in `docs/memory/text.md`, "Attribute
//! bridge"), lays the story out, and turns the placed glyphs into one path
//! per attribute run in **document space** ([`build_story`]), which the
//! walker then paints with that run's fill, stroke and transparency. A
//! gradient's geometry is in document coordinates, so one gradient spans
//! every character of a story, as in the original, without any special
//! casing here.

use std::collections::HashMap;
use std::sync::Arc;

use kurbo::{Affine, BezPath, Shape};
use xarast_doc::{
    AttrSlot, AttrValue, NodeId, NodeKind, ResolvedAttrs, StoryFlow, StoryText, TextStoryNode, Tree,
};
use xarast_geom::{Mp, Path};
use xarast_render::PathRef;
use xarast_text::{
    FontQuery, FontStyle, FontSubstitution, Justification, Layout, LineSpacing, ManualKern,
    ParagraphStyle, PathFit, PathFitStyle, StoryInput, StoryMode, StyleRange, TabKind, TabStop,
    TextPath, TextScript,
};

use crate::fonts::FontService;
use xarast_io::ExportGlyph;

/// A story ready to paint: one outline per attribute run.
#[derive(Debug, Clone)]
pub(crate) struct StoryGeometry {
    /// Runs with ink, in logical order.
    pub runs: Vec<RunGeometry>,
    /// Families substituted while laying the story out.
    pub substitutions: Vec<FontSubstitution>,
    /// Text on a path whose path is missing or has no length: drawn along
    /// a straight baseline instead.
    pub on_path_unfitted: bool,
    /// Visible characters that produced no glyph at all: no font could be
    /// found, so the story could not be drawn.
    pub unrendered: bool,
    /// A fold of the version of every node in the story, for the render
    /// cache: a character edit changes it.
    pub version: u64,
    /// The union of the runs' outlines, document space.
    pub bounds: xarast_geom::Rect,
}

/// One attribute run's glyphs.
#[derive(Debug, Clone)]
pub(crate) struct RunGeometry {
    /// Glyph outlines (and underline), document space.
    pub path: PathRef,
    /// The attributes the run paints with.
    pub attrs: ResolvedAttrs,
    /// The glyphs `path` is made of, for exporters that embed fonts.
    pub glyphs: Arc<[ExportGlyph]>,
    /// The underline bars `path` also holds, document space.
    pub decoration: Option<Arc<BezPath>>,
}

/// The shaper's input for a story: style runs, paragraph styles, manual
/// kerns and the flow.
pub(crate) struct Input {
    pub runs: Vec<StyleRange>,
    pub paragraphs: Vec<ParagraphStyle>,
    pub kerns: Vec<ManualKern>,
    pub mode: StoryMode,
}

/// The attribute bridge (`docs/memory/text.md`, "Attribute bridge").
pub(crate) fn story_input(tree: &Tree, st: &StoryText, story: &TextStoryNode) -> Input {
    let runs = st
        .runs
        .iter()
        .map(|r| style_range(r.range.clone(), &r.attrs))
        .collect();
    let paragraphs = st
        .paragraph_first_lines()
        .into_iter()
        .map(|i| {
            let line = &st.lines[i];
            paragraph_style(&line.attrs, line_ruler(tree, line.node), story.auto_kern)
        })
        .collect::<Vec<_>>();
    let paragraphs = if paragraphs.is_empty() {
        vec![paragraph_style(&st.story_attrs, None, story.auto_kern)]
    } else {
        paragraphs
    };
    let kerns = st
        .kerns
        .iter()
        .map(|k| ManualKern {
            at: k.at,
            amount: k.amount,
        })
        .collect();
    let mode = match StoryFlow::of(story) {
        StoryFlow::Point | StoryFlow::OnPath => StoryMode::Point,
        StoryFlow::Column { width, wrap } => StoryMode::Column {
            width: Mp::new(width),
            wrap,
        },
    };
    Input {
        runs,
        paragraphs,
        kerns,
        mode,
    }
}

/// The ruler a `TextLine` node carries from the file, used when no ruler
/// attribute is in force.
fn line_ruler(tree: &Tree, line: NodeId) -> Option<Arc<[xarast_doc::TabStop]>> {
    match tree.kind(line) {
        Some(NodeKind::TextLine(l)) => l.ruler.clone(),
        _ => None,
    }
}

fn style_range(range: std::ops::Range<usize>, a: &ResolvedAttrs) -> StyleRange {
    let (family, panose) = match a.get(AttrSlot::TxtFontTypeface) {
        AttrValue::FontTypeface(t) => (Arc::clone(&t.family), t.panose),
        _ => (Arc::from("Times New Roman"), None),
    };
    let bold = matches!(a.get(AttrSlot::TxtBold), AttrValue::Bold(true));
    let italic = matches!(a.get(AttrSlot::TxtItalic), AttrValue::Italic(true));
    let font = FontQuery {
        family,
        weight: if bold { 700 } else { 400 },
        style: if italic {
            FontStyle::Italic
        } else {
            FontStyle::Normal
        },
        stretch: 100,
    };
    let size = match a.get(AttrSlot::TxtFontSize) {
        AttrValue::FontSize(s) if s.raw() > 0 => *s,
        _ => Mp::new(16_000),
    };
    let mut r = StyleRange::new(range, font, size);
    r.panose = panose;
    if let AttrValue::Tracking(t) = a.get(AttrSlot::TxtTracking) {
        // The raw value is em/1000: the `Mp` type is historical.
        r.tracking = t.raw();
    }
    if let AttrValue::AspectRatio(x) = a.get(AttrSlot::TxtAspectRatio) {
        r.aspect = *x;
    }
    if let AttrValue::FontFeatures(f) = a.get(AttrSlot::TxtFeatures)
        && !f.is_empty()
    {
        r.features = f
            .iter()
            .map(|s| xarast_text::FontFeature {
                tag: s.tag,
                value: s.value,
            })
            .collect();
    }
    if let AttrValue::Baseline(b) = a.get(AttrSlot::TxtBaseline) {
        r.baseline_shift = *b;
    }
    if let AttrValue::Script(s) = a.get(AttrSlot::TxtScript) {
        r.script = if s.on {
            TextScript {
                offset: s.offset,
                size: s.size,
            }
        } else {
            TextScript::NONE
        };
    }
    r.underline = matches!(a.get(AttrSlot::TxtUnderline), AttrValue::Underline(true));
    r
}

fn paragraph_style(
    a: &ResolvedAttrs,
    line_ruler: Option<Arc<[xarast_doc::TabStop]>>,
    auto_kern: bool,
) -> ParagraphStyle {
    let mut p = ParagraphStyle {
        auto_kern,
        ..ParagraphStyle::default()
    };
    if let AttrValue::Justification(j) = a.get(AttrSlot::TxtJustification) {
        p.justification = match j {
            xarast_doc::Justification::Left => Justification::Left,
            xarast_doc::Justification::Centre => Justification::Centre,
            xarast_doc::Justification::Right => Justification::Right,
            xarast_doc::Justification::Full => Justification::Full,
        };
    }
    if let AttrValue::LineSpace(l) = a.get(AttrSlot::TxtLineSpace) {
        p.line_spacing = match l {
            xarast_doc::LineSpacing::Ratio(r) => LineSpacing::Ratio(*r),
            xarast_doc::LineSpacing::Absolute(v) => LineSpacing::Absolute(*v),
        };
    }
    if let AttrValue::LeftMargin(v) = a.get(AttrSlot::TxtLeftMargin) {
        p.left_margin = *v;
    }
    if let AttrValue::RightMargin(v) = a.get(AttrSlot::TxtRightMargin) {
        p.right_margin = *v;
    }
    if let AttrValue::FirstIndent(v) = a.get(AttrSlot::TxtFirstIndent) {
        p.first_indent = *v;
    }
    let ruler = match a.get(AttrSlot::TxtRuler) {
        AttrValue::Ruler(r) if !r.is_empty() => Some(Arc::clone(r)),
        _ => line_ruler,
    };
    if let Some(r) = ruler {
        let mut tabs: Vec<TabStop> = r
            .iter()
            .map(|t| TabStop {
                pos: t.position,
                decimal: t.decimal.unwrap_or(TabStop::DEFAULT_DECIMAL),
                kind: match t.kind & 3 {
                    1 => TabKind::Right,
                    2 => TabKind::Centre,
                    3 => TabKind::Decimal,
                    _ => TabKind::Left,
                },
            })
            .collect();
        tabs.sort_by_key(|t| t.pos);
        p.tabs = Arc::from(tabs);
    }
    p
}

/// The path a story on a path follows, in story space, ready to fit:
/// its first `Path` child (the original's rule), brought out of the
/// story's transform. `None` when the story is not on a path, has no path
/// child, the path has no length, or the transform cannot be inverted.
pub(crate) fn path_fit(tree: &Tree, story_node: NodeId, story: &TextStoryNode) -> Option<PathFit> {
    let xarast_doc::TextLayout::OnPath {
        reversed,
        tangential,
        left_indent,
        right_indent,
        chars,
    } = &story.layout
    else {
        return None;
    };
    let path = tree.children(story_node).find_map(|c| match tree.kind(c) {
        Some(NodeKind::Path(p)) => Some(Arc::clone(&p.data)),
        _ => None,
    })?;
    let xf = story.transform.to_affine();
    let det = xf.determinant();
    if !det.is_finite() || det.abs() < 1e-12 {
        return None;
    }
    let local = xf.inverse() * path.to_bez_path();
    let path = TextPath::new(&local, *reversed)?;
    Some(PathFit::new(
        path,
        PathFitStyle {
            tangential: *tangential,
            reflected: chars.reflected,
            shear: xarast_doc::CharsTransform::radians(chars.shear),
            left_indent: *left_indent,
            right_indent: *right_indent,
        },
    ))
}

/// Lays a story out: the bridge's input, the layout, and the fit onto the
/// story's path when it is on one.
pub(crate) fn lay_story(
    fonts: &FontService,
    tree: &Tree,
    st: &StoryText,
    story: &TextStoryNode,
) -> (Input, Layout, Option<PathFit>) {
    let mut input = story_input(tree, st, story);
    let fit = path_fit(tree, st.story, story);
    if let Some(f) = &fit {
        input.mode = f.story_mode();
    }
    let layout = fonts.ready().layout(&StoryInput {
        text: layout_text(st),
        runs: &input.runs,
        paragraphs: &input.paragraphs,
        kerns: &input.kerns,
        mode: input.mode,
    });
    (input, layout, fit)
}

/// Lays a story out and turns it into document-space outlines.
pub(crate) fn build_story(
    fonts: &FontService,
    tree: &Tree,
    st: &StoryText,
    story: &TextStoryNode,
) -> StoryGeometry {
    let (input, layout, fit) = lay_story(fonts, tree, st, story);
    let db = fonts.db();
    let story_xf = story.transform.to_affine();

    // Faux italic and bold: what the matched face lacks, per style run.
    let mut synth: HashMap<usize, Option<f64>> = HashMap::new();
    let mut paths: Vec<BezPath> = vec![BezPath::new(); input.runs.len()];
    // What each path is made of: its glyphs and its underline.
    let mut placed: Vec<Vec<ExportGlyph>> = vec![Vec::new(); input.runs.len()];
    let mut decor: Vec<BezPath> = vec![BezPath::new(); input.runs.len()];
    // Clusters whose text is already on a glyph.
    let mut spoken: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut glyphs = 0usize;
    // Glyphs with ink that are not the missing-glyph box: a story whose
    // visible text has none could not be drawn.
    let mut inked = 0usize;
    for line in &layout.lines {
        // On a path, each cluster has its own transform onto the path.
        let onto = |cluster: usize| -> Affine {
            match (&fit, line.cluster_at(cluster)) {
                (Some(f), Some(c)) => story_xf * f.cluster_transform(line, c),
                _ => story_xf,
            }
        };
        for run in &line.runs {
            let Some(target) = paths.get_mut(run.style) else {
                continue;
            };
            let skew = *synth.entry(run.style).or_insert_with(|| {
                let r = &input.runs[run.style];
                db.query_with_panose(&r.font, r.panose)
                    .and_then(|m| m.synthesis.skew)
                    .map(|deg| f64::from(deg).to_radians().tan())
            });
            let upem = db.units_per_em(run.face);
            let variable = run.coords.iter().any(|c| *c != 0);
            for g in &run.glyphs {
                glyphs += 1;
                let text: Arc<str> = if spoken.insert(g.cluster) {
                    line.cluster_at(g.cluster)
                        .and_then(|c| st.text.get(c.range.clone()))
                        .map_or_else(|| Arc::from(""), Arc::from)
                } else {
                    Arc::from("")
                };
                let Some(outline) = db.glyph_outline_normalized(run.face, g.id, &run.coords) else {
                    continue;
                };
                if g.id != 0 && !outline.elements().is_empty() {
                    inked += 1;
                }
                let mut xf = run.glyph_transform(g, upem);
                if let Some(k) = skew {
                    // Shear about the glyph's own baseline.
                    let (x, y) = (g.x.to_f64(), g.y.to_f64());
                    xf = Affine::translate((x, y))
                        * Affine::new([1.0, 0.0, k, 1.0, 0.0, 0.0])
                        * Affine::translate((-x, -y))
                        * xf;
                }
                let xf = onto(g.cluster) * xf;
                for el in outline.elements() {
                    target.push(xf * *el);
                }
                if let Some(p) = placed.get_mut(run.style) {
                    p.push(ExportGlyph {
                        face: run.face,
                        id: g.id,
                        transform: xf,
                        variable,
                        text,
                    });
                }
            }
            if input.runs[run.style].underline && !run.glyphs.is_empty() {
                // Position and thickness are the usual typographic defaults
                // (a tenth of the size below the baseline, a twentieth
                // thick): the original's own values come from the font.
                let size = run.size.to_f64();
                let y = line.baseline_y.to_f64() - size * 0.1;
                let under = &mut decor[run.style];
                let mut bar = |x0: f64, x1: f64, xf: Affine| {
                    let rect = kurbo::Rect::new(x0, y - size * 0.05, x1, y);
                    for el in rect.path_elements(0.1) {
                        target.push(xf * el);
                        under.push(xf * el);
                    }
                };
                if fit.is_some() {
                    // Along a path the bar is broken into one piece per
                    // character, each turned with its character.
                    let mut last = None;
                    for g in &run.glyphs {
                        if last == Some(g.cluster) {
                            continue;
                        }
                        last = Some(g.cluster);
                        if let Some(c) = line.cluster_at(g.cluster) {
                            bar(c.x.to_f64(), (c.x + c.width).to_f64(), onto(g.cluster));
                        }
                    }
                } else {
                    let first = &run.glyphs[0];
                    let last = &run.glyphs[run.glyphs.len() - 1];
                    let x0 = first.x.to_f64();
                    let x1 = last.x.to_f64() + last.advance.to_f64();
                    bar(x0, x1, story_xf);
                }
            }
        }
    }
    let visible = st.text.chars().any(|c| !c.is_whitespace());
    let mut bounds = xarast_geom::Rect::EMPTY;
    for p in &paths {
        if p.elements().is_empty() {
            continue;
        }
        let b = p.bounding_box();
        let r = xarast_geom::Rect::new(
            xarast_geom::Point::new(Mp::from_f64_round(b.x0), Mp::from_f64_round(b.y0)),
            xarast_geom::Point::new(Mp::from_f64_round(b.x1), Mp::from_f64_round(b.y1)),
        );
        bounds = if bounds.is_empty() {
            r
        } else {
            bounds.union(r)
        };
    }
    let runs = paths
        .into_iter()
        .zip(placed)
        .zip(decor)
        .zip(&st.runs)
        .filter(|(((p, _), _), _)| !p.elements().is_empty())
        .map(|(((p, g), d), r)| RunGeometry {
            path: PathRef::new(Path::from_bez_path(&p).0),
            attrs: r.attrs.clone(),
            glyphs: Arc::from(g),
            decoration: (!d.elements().is_empty()).then(|| Arc::new(d)),
        })
        .collect();
    StoryGeometry {
        runs,
        substitutions: layout.substitutions,
        on_path_unfitted: matches!(StoryFlow::of(story), StoryFlow::OnPath) && fit.is_none(),
        unrendered: visible && (glyphs == 0 || inked == 0),
        version: subtree_version(tree, st.story),
        bounds,
    }
}

/// A story laid out for "Convert to shapes" (T9.6.3): one outline per
/// attribute run with the attributes it paints with, exactly the geometry
/// the walker draws, plus the story's logical text. `None` when `story` is
/// not a text story or nothing of it would be drawn (no ink, or no font).
pub(crate) fn story_outlines(
    fonts: &FontService,
    doc: &xarast_doc::Document,
    story: NodeId,
) -> Option<(Vec<xarast_doc::OutlineRun>, String)> {
    let Some(NodeKind::TextStory(node)) = doc.tree.kind(story) else {
        return None;
    };
    let mut stack = xarast_doc::attr::resolve_inherited(&doc.tree, story, &doc.defaults);
    let st = StoryText::collect(&doc.tree, story, &mut stack, &mut |_, a| {
        Arc::new(a.value.clone())
    })?;
    let g = build_story(fonts, &doc.tree, &st, node);
    if g.unrendered || g.runs.is_empty() {
        return None;
    }
    let runs = g
        .runs
        .into_iter()
        .map(|r| xarast_doc::OutlineRun {
            path: Arc::new(r.path.path().clone()),
            attrs: r.attrs,
        })
        .collect();
    Some((runs, layout_text(&st).to_owned()))
}

/// The families of the faces `story` is drawn with whose licence
/// (`OS/2.fsType`) forbids embedding them, each once; empty when every face
/// may be embedded or `story` is not a story.
pub(crate) fn story_refused_faces(
    fonts: &FontService,
    doc: &xarast_doc::Document,
    story: NodeId,
) -> Vec<Arc<str>> {
    let Some(NodeKind::TextStory(node)) = doc.tree.kind(story) else {
        return Vec::new();
    };
    let mut stack = xarast_doc::attr::resolve_inherited(&doc.tree, story, &doc.defaults);
    let Some(st) = StoryText::collect(&doc.tree, story, &mut stack, &mut |_, a| {
        Arc::new(a.value.clone())
    }) else {
        return Vec::new();
    };
    let (_, layout, _) = lay_story(fonts, &doc.tree, &st, node);
    let db = fonts.db();
    let mut out: Vec<Arc<str>> = Vec::new();
    for line in &layout.lines {
        for run in &line.runs {
            if db.embedding_denied(run.face)
                && let Some(info) = db.face_info(run.face)
                && !out.contains(&info.family)
            {
                out.push(info.family);
            }
        }
    }
    out
}

/// The document-space box a story's lines occupy (advance boxes, not ink),
/// or an empty rectangle when it holds no text. `attrs` is the state in
/// force at the story, as for [`StoryText::collect`].
pub(crate) fn story_rect(
    fonts: &FontService,
    tree: &Tree,
    story_node: NodeId,
    attrs: &mut xarast_doc::AttrStack,
) -> xarast_geom::Rect {
    let Some(NodeKind::TextStory(story)) = tree.kind(story_node) else {
        return xarast_geom::Rect::EMPTY;
    };
    let Some(st) = StoryText::collect(tree, story_node, attrs, &mut |_, a| {
        Arc::new(a.value.clone())
    }) else {
        return xarast_geom::Rect::EMPTY;
    };
    if st.text.trim().is_empty() {
        return xarast_geom::Rect::EMPTY;
    }
    let (_, layout, fit) = lay_story(fonts, tree, &st, story);
    if layout.bounds.is_empty() {
        return xarast_geom::Rect::EMPTY;
    }
    let Some(fit) = fit else {
        return story.transform.transform_rect(layout.bounds);
    };
    // On a path: every cluster's line box, carried onto the path.
    let story_xf = story.transform.to_affine();
    let mut out: Option<kurbo::Rect> = None;
    for line in &layout.lines {
        let top = (line.baseline_y + line.ascent).to_f64();
        let bottom = (line.baseline_y - line.descent).to_f64();
        for c in &line.clusters {
            let xf = story_xf * fit.cluster_transform(line, c);
            let (x0, x1) = (c.x.to_f64(), (c.x + c.width).to_f64());
            for (x, y) in [(x0, top), (x1, top), (x0, bottom), (x1, bottom)] {
                let q = xf * kurbo::Point::new(x, y);
                out = Some(match out {
                    Some(r) => r.union_pt(q),
                    None => kurbo::Rect::from_points(q, q),
                });
            }
        }
    }
    let Some(r) = out else {
        return xarast_geom::Rect::EMPTY;
    };
    xarast_geom::Rect::new(
        xarast_geom::Point::new(Mp::from_f64_round(r.x0), Mp::from_f64_round(r.y0)),
        xarast_geom::Point::new(Mp::from_f64_round(r.x1), Mp::from_f64_round(r.y1)),
    )
}

/// The text layout sees: the logical text without the paragraph break
/// that ends the story. Every line of a `.xar` story ends with an EOL, so
/// the last one would otherwise open an empty paragraph after the text,
/// which the original never draws or measures.
pub(crate) fn layout_text(st: &StoryText) -> &str {
    st.text.strip_suffix('\n').unwrap_or(&st.text)
}

/// A fold of the tag and content revision of every node under `root`.
fn subtree_version(tree: &Tree, root: NodeId) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for n in tree.preorder(root) {
        let tag = u64::from(tree.get(n).map_or(0, |d| d.tag.0));
        let v = (tag << 32) ^ tree.content_rev(n);
        h = (h ^ v).wrapping_mul(0x0100_0000_01b3);
        h ^= h >> 29;
    }
    h
}

/// The story as if `text` had been typed at byte `at`: the input method's
/// preedit, drawn and laid out but not in the document (T9.4.7).
///
/// The inserted text takes the style typing there would give it: the
/// character before `at`, or the one at `at` at the start of the story or
/// of a paragraph. Everything after `at` moves along; `at` past the text
/// or off a character boundary changes nothing.
pub(crate) fn splice_text(st: &StoryText, at: usize, text: &str) -> StoryText {
    let mut out = st.clone();
    if text.is_empty() || at > st.text.len() || !st.text.is_char_boundary(at) {
        return out;
    }
    let len = text.len();
    let wide = u32::try_from(len).unwrap_or(u32::MAX);
    let after_break = at == 0 || st.text[..at].ends_with('\n');
    // The run whose style the text takes.
    let style = if after_break {
        st.runs.iter().position(|r| r.range.contains(&at))
    } else {
        st.runs.iter().position(|r| r.range.contains(&(at - 1)))
    };
    out.text.insert_str(at, text);
    if let Some(k) = style {
        for (i, r) in out.runs.iter_mut().enumerate() {
            if i == k {
                r.range.end += len;
            } else if i > k {
                r.range.start += len;
                r.range.end += len;
            }
        }
    } else {
        // No run covers the place (an empty story's text): one run in the
        // story's attributes.
        for r in &mut out.runs {
            if r.range.start >= at {
                r.range.start += len;
                r.range.end += len;
            }
        }
        let i = out.runs.partition_point(|r| r.range.start < at);
        out.runs.insert(
            i,
            xarast_doc::CharRun {
                range: at..at + len,
                attrs: st.story_attrs.clone(),
            },
        );
    }
    for l in &mut out.lines {
        if l.first_byte as usize > at {
            l.first_byte = l.first_byte.saturating_add(wide);
        }
    }
    for item in &mut out.items {
        if item.byte as usize >= at {
            item.byte = item.byte.saturating_add(wide);
        }
    }
    for k in &mut out.kerns {
        if k.at >= at {
            k.at += len;
        }
    }
    out
}

#[cfg(test)]
mod splice_tests {
    use super::*;

    use xarast_doc::builder::{BuildLimits, skeleton};
    use xarast_doc::{AttrSlot, AttrValue, NodeKind, TextItem};

    /// "ab" at 20 pt then "cd" at 12 pt and a break; "ef" and the final
    /// break on a second line.
    fn story() -> StoryText {
        let mut b = skeleton(BuildLimits::default()).unwrap();
        let story = b
            .node(NodeKind::TextStory(Box::default()))
            .unwrap()
            .node_id();
        b.push_scope().unwrap();
        b.attribute(AttrValue::FontSize(Mp::new(20_000))).unwrap();
        b.node(NodeKind::TextLine(Box::default())).unwrap();
        b.push_scope().unwrap();
        for c in ['a', 'b'] {
            b.node(NodeKind::TextItem(TextItem::Char(c))).unwrap();
        }
        b.attribute(AttrValue::FontSize(Mp::new(12_000))).unwrap();
        for c in ['c', 'd'] {
            b.node(NodeKind::TextItem(TextItem::Char(c))).unwrap();
        }
        b.node(NodeKind::TextItem(TextItem::LineBreak(true)))
            .unwrap();
        b.pop_scope();
        b.node(NodeKind::TextLine(Box::default())).unwrap();
        b.push_scope().unwrap();
        for c in ['e', 'f'] {
            b.node(NodeKind::TextItem(TextItem::Char(c))).unwrap();
        }
        b.node(NodeKind::TextItem(TextItem::LineBreak(true)))
            .unwrap();
        b.pop_scope();
        b.pop_scope();
        let (doc, _) = b.finish().unwrap();
        crate::text_clip::story_text(&doc, story).unwrap()
    }

    fn size_at(st: &StoryText, b: usize) -> AttrValue {
        let run = st.runs.iter().find(|r| r.range.contains(&b)).unwrap();
        run.attrs.get(AttrSlot::TxtFontSize).clone()
    }

    #[test]
    fn a_splice_takes_the_style_before_it_and_moves_the_rest() {
        let st = story();
        assert_eq!(st.text, "abcd\nef\n");
        // After "abc": the 12 pt style of "c".
        let s = splice_text(&st, 3, "XY");
        assert_eq!(s.text, "abcXYd\nef\n");
        assert_eq!(size_at(&s, 3), AttrValue::FontSize(Mp::new(12_000)));
        assert_eq!(size_at(&s, 1), AttrValue::FontSize(Mp::new(20_000)));
        assert_eq!(s.runs.last().unwrap().range.end, s.text.len());
        assert_eq!(s.lines[1].first_byte as usize, 7);
        // At a paragraph's start: the style of the character after it,
        // and the line keeps its start.
        let s = splice_text(&st, 5, "Z");
        assert_eq!(s.text, "abcd\nZef\n");
        assert_eq!(s.lines[1].first_byte as usize, 5);
        assert_eq!(size_at(&s, 5), size_at(&st, 5));
        // Off a boundary or past the end: unchanged.
        assert_eq!(splice_text(&st, 99, "Z").text, st.text);
    }
}
