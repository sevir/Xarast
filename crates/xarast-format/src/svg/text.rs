//! Text stories in the profile (`research/06 §6.7`): what the writer, the
//! reader and the normal form share.
//!
//! A story is written as `<text xarast:exact="true">` holding one `<tspan>`
//! per `TextLine`, and inside each line one `<tspan>` per **run**: a
//! maximal sequence of the line's items whose resolved attributes write
//! the same (text attributes, paint and its twins). The run's start tag
//! carries everything the model needs — the text attributes as SVG
//! properties plus `xarast:` twins for what SVG has no property for — and
//! its content is the items in order: characters as text, and
//! `<xarast:kern>`, `<xarast:eol>` and `<xarast:char>` for the items that
//! are not characters a browser should draw. A reader rebuilds the line's
//! items in order and, before each run, the attributes that differ from
//! the previous run's (the model then resolves every item exactly as it
//! did before the save).
//!
//! The *base* SVG places the characters where the application's layout
//! does when the writer is given a [`TextPlacer`] (per-character `x` / `y`
//! lists on each run); without one, each line starts at the story's origin,
//! one line height below the previous one, which is what the profile wrote
//! before Phase 9.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use xarast_doc::{AttrSlot, AttrStack, AttrValue, Document, NodeId, TabStop};
use xarast_geom::Mp;

use super::num::{f32s, mp};

/// Where the application's layout puts a story's characters, for the base
/// SVG of text (`research/06 §6.7`). The writer knows no fonts: the
/// application, which lays text out to draw it, provides this.
pub trait TextPlacer: Send + Sync {
    /// Places the characters of the story `story`. `attrs` is the attribute
    /// state in force at the story, as a render walk has it on visiting the
    /// story; an implementation must leave it as it found it. `None` when
    /// the story cannot be laid out (the writer then falls back to one line
    /// per `TextLine` at the story's origin).
    fn place(&self, doc: &Document, story: NodeId, attrs: &mut AttrStack)
    -> Option<StoryPlacement>;

    /// The font file to embed for `face` (one of the faces a
    /// [`StoryPlacement`] listed), covering `chars`: every character the
    /// document draws with it (`research/06 §6.7` rules 2–3). `None`: this
    /// placer embeds no fonts.
    fn font_file(&self, face: &PlacedFace, chars: &[char]) -> Option<FontFile> {
        let _ = (face, chars);
        None
    }

    /// The story `story` laid out as outlines, one per attribute run, in
    /// document space, with the attributes each paints with: what a mould
    /// bakes its text from (`research/06 §6.8.5`: text under a mould is
    /// written as curves). `None`: this placer makes no outlines, and the
    /// writer leaves the text out of the baked copy.
    fn outlines(&self, doc: &Document, story: NodeId) -> Option<Vec<xarast_doc::OutlineRun>> {
        let _ = (doc, story);
        None
    }
}

/// A face placed text is drawn with, as a browser should know it for
/// `@font-face`. Ordered, so the rules come out in a fixed order.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PlacedFace {
    /// The face's own family name.
    pub family: Arc<str>,
    /// CSS weight (400 regular, 700 bold).
    pub weight: u16,
    /// Italic or oblique.
    pub italic: bool,
    /// The placer's own name for the face, stable for one write.
    pub key: u64,
}

/// What [`TextPlacer::font_file`] gives for a face.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FontFile {
    /// A WOFF2 file holding (at least) the characters asked for.
    Woff2(Arc<[u8]>),
    /// The face's licence (`OS/2.fsType`) forbids embedding it: no file,
    /// and its runs say `xarast:font-embed="denied"`.
    Denied(Arc<str>),
    /// Embedding failed for another reason (why).
    Unavailable(Arc<str>),
}

/// A laid-out story, reduced to what the SVG base needs.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StoryPlacement {
    /// The position of each character item (`TextItem::Char` or `Tab`) in
    /// story space (y up, the first baseline at 0): the left edge of its
    /// box on its baseline, shifts included; for a story placed along its
    /// path, the origin of its glyphs on the path. Items not listed are
    /// placed after the previous one.
    pub chars: HashMap<NodeId, (Mp, Mp)>,
    /// Families that were not available, with the family used instead.
    pub substitutions: Vec<(Arc<str>, Arc<str>)>,
    /// Whether a story on a path was placed along its path (T9.5.6): each
    /// character at its own position, turned by its entry in `rotations`.
    /// `false` for a story on a path laid out on straight lines.
    pub along_path: bool,
    /// For a story placed along its path, the turn of each character item
    /// about its position, in degrees counter-clockwise (y up). Items not
    /// listed do not turn.
    pub rotations: HashMap<NodeId, f64>,
    /// The faces the story is drawn with, each with the characters drawn
    /// with it, for the embedded fonts (`research/06 §6.7` rule 2).
    pub faces: Vec<(PlacedFace, String)>,
    /// For each character item, the index in `faces` of the face its
    /// glyphs are drawn with.
    pub char_faces: HashMap<NodeId, usize>,
    /// Families (as the document names them) whose face refuses embedding:
    /// their runs carry `xarast:font-embed="denied"` (rule 3).
    pub denied: Vec<Arc<str>>,
}

/// A shared [`TextPlacer`] in [`super::SvgOptions`], compared by identity.
#[derive(Clone)]
pub struct Placer(pub Arc<dyn TextPlacer>);

impl fmt::Debug for Placer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Placer(..)")
    }
}

impl PartialEq for Placer {
    fn eq(&self, other: &Placer) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for Placer {}

/// A ruler as the profile writes it: `position:kind` pairs, the position
/// in points and the kind the format's `type_and_flags` byte.
pub(crate) fn ruler_text(stops: &[TabStop]) -> String {
    let v: Vec<String> = stops
        .iter()
        .map(|t| format!("{}:{}", mp(i64::from(t.position.raw())), t.kind))
        .collect();
    v.join(" ")
}

/// The generic family a browser falls back to, from the PANOSE bytes the
/// way the application's substitution ladder reads them: family kind 2
/// (Latin text) with proportion 9 is monospaced; serif styles 11–13 are
/// sans serif, the others serif. Without PANOSE, `sans-serif`.
pub(crate) fn generic_family(panose: Option<[u8; 10]>) -> &'static str {
    match panose {
        Some(p) if p[0] == 2 && p[3] == 9 => "monospace",
        Some(p) if p[0] == 2 && (11..=13).contains(&p[1]) => "sans-serif",
        Some(p) if p[0] == 2 && p[1] >= 2 => "serif",
        _ => "sans-serif",
    }
}

/// The attributes a run's start tag carries for its text attributes, in
/// the order written: the SVG properties a browser uses, then the twins
/// (`research/06 §6.7`). `substitute` is the family the application used
/// instead of the requested one, when it had to. `denied` are the
/// requested families whose face refuses embedding (the faces actually
/// drawn with join the chain per run, [`chain_with`]).
///
/// Every value here is resolved from `a`; a reader rebuilds the model's
/// values from these alone (`read/build/ink.rs`).
pub(crate) fn run_text_attrs(
    a: &AttrStack,
    substitutes: &[(Arc<str>, Arc<str>)],
    denied: &[Arc<str>],
) -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = Vec::with_capacity(8);
    let (family, full, panose) = match a.get(AttrSlot::TxtFontTypeface) {
        AttrValue::FontTypeface(t) => (Arc::clone(&t.family), Arc::clone(&t.full_name), t.panose),
        _ => (Arc::from(""), Arc::from(""), None),
    };
    let size = match a.get(AttrSlot::TxtFontSize) {
        AttrValue::FontSize(v) => i64::from(v.raw()),
        _ => 0,
    };
    let script = match a.get(AttrSlot::TxtScript) {
        AttrValue::Script(s) => *s,
        _ => xarast_doc::Script::default(),
    };
    // What a browser should draw at: the script's size applies.
    let drawn = if script.on && script.size.is_finite() && script.size > 0.0 {
        (size as f64 * f64::from(script.size)).round() as i64
    } else {
        size
    };
    let substitute = substitutes
        .iter()
        .find(|(from, _)| **from == *family)
        .map(|(_, to)| Arc::clone(to));
    let mut chain = String::new();
    let mut named: Vec<String> = Vec::new();
    let first = [Some(&family), substitute.as_ref()];
    for f in first.into_iter().flatten() {
        let f = f.replace(['\'', '"', ';', '\\'], "");
        let f = f.trim();
        if !f.is_empty() && !named.iter().any(|n| n == f) {
            chain.push('\'');
            chain.push_str(f);
            chain.push_str("', ");
            named.push(f.to_owned());
        }
    }
    chain.push_str(generic_family(panose));
    out.push(("font-family", chain));
    out.push(("font-size", mp(drawn)));
    if let AttrValue::Bold(true) = a.get(AttrSlot::TxtBold) {
        out.push(("font-weight", "bold".into()));
    }
    if let AttrValue::Italic(true) = a.get(AttrSlot::TxtItalic) {
        out.push(("font-style", "italic".into()));
    }
    if let AttrValue::Underline(true) = a.get(AttrSlot::TxtUnderline) {
        out.push(("text-decoration", "underline".into()));
    }
    // The twins.
    // The first name of `font-family` is the family whenever the chain can
    // spell it; otherwise the twin says it.
    if family.trim().is_empty()
        || family.contains(['\'', '"', ';', '\\', ','])
        || family.trim() != &*family
    {
        out.push(("xarast:family", family.to_string()));
    }
    if *family != *full {
        out.push(("xarast:font", full.to_string()));
    }
    if let Some(p) = panose {
        let hex: String = p.iter().map(|b| format!("{b:02x}")).collect();
        out.push(("xarast:panose", hex));
    }
    if let Some(s) = &substitute {
        out.push(("xarast:font-substitute", s.to_string()));
    }
    if denied.contains(&family) {
        out.push(("xarast:font-embed", "denied".into()));
    }
    if drawn != size {
        out.push(("xarast:size", mp(size)));
    }
    if let AttrValue::AspectRatio(x) = a.get(AttrSlot::TxtAspectRatio)
        && x.to_bits() != 1.0f32.to_bits()
    {
        out.push(("xarast:aspect", f32s(*x)));
    }
    if let AttrValue::Tracking(t) = a.get(AttrSlot::TxtTracking)
        && t.raw() != 0
    {
        // Thousandths of an em, not millipoints (`docs/memory/text.md`).
        out.push(("xarast:tracking", t.raw().to_string()));
    }
    if script != xarast_doc::Script::default() {
        out.push((
            "xarast:script",
            format!(
                "{} {} {}",
                if script.on { "on" } else { "off" },
                f32s(script.offset),
                f32s(script.size)
            ),
        ));
    }
    if let AttrValue::Baseline(b) = a.get(AttrSlot::TxtBaseline)
        && b.raw() != 0
    {
        out.push(("xarast:baseline", mp(i64::from(b.raw()))));
    }
    match a.get(AttrSlot::TxtJustification) {
        AttrValue::Justification(xarast_doc::Justification::Centre) => {
            out.push(("xarast:justify", "centre".into()));
        }
        AttrValue::Justification(xarast_doc::Justification::Right) => {
            out.push(("xarast:justify", "right".into()));
        }
        AttrValue::Justification(xarast_doc::Justification::Full) => {
            out.push(("xarast:justify", "full".into()));
        }
        _ => {}
    }
    match a.get(AttrSlot::TxtLineSpace) {
        AttrValue::LineSpace(xarast_doc::LineSpacing::Ratio(r))
            if r.to_bits() != 1.0f32.to_bits() =>
        {
            out.push(("xarast:line-spacing", format!("ratio:{}", f32s(*r))));
        }
        AttrValue::LineSpace(xarast_doc::LineSpacing::Absolute(v)) => {
            out.push((
                "xarast:line-spacing",
                format!("abs:{}", mp(i64::from(v.raw()))),
            ));
        }
        _ => {}
    }
    for (slot, name) in [
        (AttrSlot::TxtLeftMargin, "xarast:indent-left"),
        (AttrSlot::TxtRightMargin, "xarast:indent-right"),
        (AttrSlot::TxtFirstIndent, "xarast:indent-first"),
    ] {
        if let AttrValue::LeftMargin(v) | AttrValue::RightMargin(v) | AttrValue::FirstIndent(v) =
            a.get(slot)
            && v.raw() != 0
        {
            out.push((name, mp(i64::from(v.raw()))));
        }
    }
    if let AttrValue::Ruler(r) = a.get(AttrSlot::TxtRuler)
        && !r.is_empty()
    {
        out.push(("xarast:ruler", ruler_text(r)));
    }
    if let AttrValue::FontFeatures(f) = a.get(AttrSlot::TxtFeatures)
        && !f.is_empty()
    {
        out.push(("xarast:features", features_text(f)));
    }
    out
}

/// OpenType feature settings as `xarast:features` spells them: `tag:value`
/// pairs separated by spaces, in tag order (`liga:0 smcp:1`).
pub(crate) fn features_text(f: &[xarast_doc::FeatureSetting]) -> String {
    f.iter()
        .map(|s| format!("{}:{}", s.tag_str(), s.value))
        .collect::<Vec<_>>()
        .join(" ")
}

/// A `font-family` chain with `families` inserted after its first name
/// (the family the document asks for), each once.
pub(crate) fn chain_with(chain: &str, families: &[&str]) -> String {
    // Split at the commas outside quotes: a quoted family may hold one.
    let mut names: Vec<&str> = Vec::new();
    let (mut start, mut quoted_run) = (0usize, false);
    for (i, c) in chain.char_indices() {
        match c {
            '\'' => quoted_run = !quoted_run,
            ',' if !quoted_run => {
                names.push(chain[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    names.push(chain[start..].trim());
    let mut out: Vec<String> = Vec::with_capacity(names.len() + families.len());
    let quoted = |f: &str| {
        let f = f.replace(['\'', '"', ';', '\\'], "");
        format!("'{}'", f.trim())
    };
    let push = |n: String, out: &mut Vec<String>| {
        if n != "''" && !out.contains(&n) {
            out.push(n);
        }
    };
    let mut rest = names.iter();
    if let Some(first) = rest.next() {
        push((*first).to_owned(), &mut out);
    }
    for f in families {
        push(quoted(f), &mut out);
    }
    for n in rest {
        push((*n).to_owned(), &mut out);
    }
    out.join(", ")
}

/// The `@font-face` rule for an embedded face whose file is at `href` (a
/// package path or a `data:` URI; neither holds a quote or a parenthesis).
pub(crate) fn font_face_rule(face: &PlacedFace, href: &str) -> String {
    let family = face
        .family
        .replace(['\'', '"', ';', '\\', '{', '}', '<', '>', '&'], "");
    format!(
        "@font-face{{font-family:'{}';font-weight:{};font-style:{};src:url({}) format('woff2');}}",
        family.trim(),
        face.weight,
        if face.italic { "italic" } else { "normal" },
        href
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn faces_join_the_chain_after_the_family_asked_for() {
        assert_eq!(
            chain_with(
                "'Arial', 'Noto Sans', sans-serif",
                &["Noto Sans Hebrew", "Noto Sans"]
            ),
            "'Arial', 'Noto Sans Hebrew', 'Noto Sans', sans-serif"
        );
        // A quoted family holding a comma stays one name.
        assert_eq!(chain_with("'A, B', serif", &["C"]), "'A, B', 'C', serif");
        assert_eq!(chain_with("sans-serif", &["X"]), "sans-serif, 'X'");
    }

    #[test]
    fn a_font_face_rule_names_the_face_and_its_file() {
        let face = PlacedFace {
            family: Arc::from("Noto Sans"),
            weight: 700,
            italic: true,
            key: 3,
        };
        assert_eq!(
            font_face_rule(&face, "resources/fonts/b3-0.woff2"),
            "@font-face{font-family:'Noto Sans';font-weight:700;font-style:italic;\
             src:url(resources/fonts/b3-0.woff2) format('woff2');}"
        );
    }
}
