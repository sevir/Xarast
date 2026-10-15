//! `xarast-cli inspect --fills`: every fill in a document (phase 8 T8.8.4).
//!
//! One row per fill **attribute** in the document tree — colour fill,
//! transparency, line colour, line transparency — with its shape, how many
//! intermediate stops its ramp has, its bias/gain profile, the ramp
//! mapping, perspective, the transparency mode, and the fill effect and
//! mapping attributes that modify it. Then a histogram by slot and shape.
//!
//! The histogram is what phase 8's acceptance criterion 3 compares with
//! `xar-dump --tags`: a `.xar` fill record becomes exactly one attribute of
//! the model, so the counts must agree tag family by tag family
//! ([`tag_census`] is the `.xar` side of that comparison).
//!
//! # Clean room
//!
//! Only facts are printed: kinds, counts and the profile's two numbers,
//! never a coordinate or a colour from the file, so the output of a
//! corpus file is safe to paste into an issue.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use xarast_app::{DocumentId, Session};
use xarast_color::{FillEffect, Stop, TranspMode};
use xarast_doc::fill::{FillGeometry, RampMapping, Tiling};
use xarast_doc::{AttrValue, Document, NodeId, NodeKind};
use xarast_geom::BiasGain;

use crate::Exit;
use crate::args::Args;
use crate::inputs::expand;

/// Usage for `inspect`.
pub const USAGE: &str = "\
xarast-cli inspect — report what a document holds

USAGE:
    xarast-cli inspect --fills <IN.xar|IN.xarast|DIR>... [--summary]

A directory stands for every .xar file below it.

REPORTS
    --fills     every fill attribute: colour fill, transparency, line
                colour and line transparency, with its shape, the number of
                intermediate ramp stops, the bias/gain profile, the ramp
                mapping, perspective, the transparency mode, and the fill
                effect and mapping attributes that modify it; then the
                histogram by slot and shape

OPTIONS
    --summary   print only the histogram

Only facts are printed (kinds, counts, profiles), never coordinates or
colours from the file.

EXIT CODE
    0 when every input opened, 2 when any did not.
";

/// Parsed `inspect` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InspectArgs {
    /// Files or directories.
    pub inputs: Vec<PathBuf>,
    /// Print only the histogram.
    pub summary: bool,
}

/// Parses `inspect`'s arguments.
///
/// # Errors
///
/// A message for the user when the command line is wrong.
pub fn parse(argv: &[String]) -> Result<InspectArgs, String> {
    let mut a = InspectArgs {
        inputs: Vec::new(),
        summary: false,
    };
    let mut fills = false;
    let mut it = Args::new(argv);
    while let Some(arg) = it.next_arg() {
        match arg.as_str() {
            "--fills" => fills = true,
            "--summary" => a.summary = true,
            other if other.starts_with('-') && other.len() > 1 => {
                return Err(format!("unknown option {other}"));
            }
            other => a.inputs.push(PathBuf::from(other)),
        }
    }
    if !fills {
        return Err("say what to inspect: --fills".into());
    }
    if a.inputs.is_empty() {
        return Err("no input given".into());
    }
    Ok(a)
}

/// Which paint slot a fill attribute occupies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FillSlot {
    /// The colour fill.
    Fill,
    /// The fill transparency.
    Transparency,
    /// The line colour.
    Line,
    /// The line transparency.
    LineTransparency,
}

impl FillSlot {
    /// The name printed for the slot.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            FillSlot::Fill => "fill",
            FillSlot::Transparency => "transparency",
            FillSlot::Line => "line",
            FillSlot::LineTransparency => "line-transparency",
        }
    }
}

/// One fill attribute.
#[derive(Debug, Clone, PartialEq)]
pub struct FillRow {
    /// The slot.
    pub slot: FillSlot,
    /// The shape: `flat`, `linear`, `circular`, `elliptical`, `conical`,
    /// `diamond`, `three-colour`, `four-colour`, `bitmap`, `contone`,
    /// `fractal` or `noise`.
    pub shape: &'static str,
    /// Intermediate ramp stops (the endpoints are not counted).
    pub stops: usize,
    /// The bias/gain profile, identity when the fill has none.
    pub profile: BiasGain,
    /// Whether the ramp parameter is eased.
    pub mapping: RampMapping,
    /// Whether the fill carries perspective corners.
    pub perspective: bool,
    /// A transparency's mode.
    pub mode: Option<TranspMode>,
    /// The fill effect attribute that follows the fill, if any.
    pub effect: Option<FillEffect>,
    /// The fill mapping attribute that follows the fill, if any.
    pub tiling: Option<Tiling>,
    /// Whether the fill sits inside a line of text (a run attribute).
    pub in_text: bool,
}

/// Every fill attribute of a document, in tree order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FillCensus {
    /// The fills.
    pub rows: Vec<FillRow>,
    /// Fill effect attributes, by name, whether or not a fill precedes
    /// them.
    pub effects: BTreeMap<&'static str, usize>,
    /// Fill mapping attributes, by `fill`/`transparency` and name.
    pub mappings: BTreeMap<(&'static str, &'static str), usize>,
    /// Fill effect and mapping attributes inside lines of text, which the
    /// two maps above leave out.
    pub text_modifiers: usize,
}

impl FillCensus {
    /// `(slot, shape) → count`, over every fill.
    #[must_use]
    pub fn histogram(&self) -> BTreeMap<(&'static str, &'static str), usize> {
        self.histogram_where(|_| true)
    }

    /// `(slot, shape) → count`, over the fills outside lines of text.
    ///
    /// Inside a line, the `.xar` format hangs attributes off each string
    /// and the text importer folds them into runs: a value equal to the one
    /// in force is not repeated, and the end of a run restores the line's
    /// value. The counts there are the text mapping's, not one record per
    /// attribute.
    #[must_use]
    pub fn histogram_outside_text(&self) -> BTreeMap<(&'static str, &'static str), usize> {
        self.histogram_where(|r| !r.in_text)
    }

    fn histogram_where(
        &self,
        keep: impl Fn(&FillRow) -> bool,
    ) -> BTreeMap<(&'static str, &'static str), usize> {
        let mut h = BTreeMap::new();
        for r in self.rows.iter().filter(|r| keep(r)) {
            *h.entry((r.slot.name(), r.shape)).or_insert(0) += 1;
        }
        h
    }
}

/// The name of a fill's shape.
#[must_use]
pub fn shape_name<S: Stop>(g: &FillGeometry<S>) -> &'static str {
    match g {
        FillGeometry::Flat { .. } => "flat",
        FillGeometry::Linear { .. } => "linear",
        FillGeometry::Radial {
            aspect_locked: true,
            ..
        } => "circular",
        FillGeometry::Radial { .. } => "elliptical",
        FillGeometry::Conical { .. } => "conical",
        FillGeometry::Diamond { .. } => "diamond",
        FillGeometry::ThreeColour { .. } => "three-colour",
        FillGeometry::FourColour { .. } => "four-colour",
        FillGeometry::Bitmap {
            contone: Some(_), ..
        } => "contone",
        FillGeometry::Bitmap { .. } => "bitmap",
        FillGeometry::Fractal { .. } => "fractal",
        FillGeometry::Noise { .. } => "noise",
    }
}

fn row<S: Stop>(slot: FillSlot, g: &FillGeometry<S>, mode: Option<TranspMode>) -> FillRow {
    let (stops, mapping, perspective) = match g {
        FillGeometry::Linear { ramp, persp, .. }
        | FillGeometry::Radial { ramp, persp, .. }
        | FillGeometry::Diamond { ramp, persp, .. } => {
            (ramp.stops().len(), ramp.mapping, persp.is_some())
        }
        FillGeometry::Conical { ramp, .. } => (ramp.stops().len(), ramp.mapping, false),
        FillGeometry::Bitmap { persp, .. } => (0, RampMapping::Linear, persp.is_some()),
        _ => (0, RampMapping::Linear, false),
    };
    // A bitmap transparency's pair is its two levels, not a contone: it is
    // still a bitmap transparency, as the file's own record says.
    let shape = match shape_name(g) {
        "contone" if mode.is_some() => "bitmap",
        s => s,
    };
    FillRow {
        slot,
        shape,
        stops,
        profile: g.profile(),
        mapping,
        perspective,
        mode,
        effect: None,
        tiling: None,
        in_text: false,
    }
}

const fn effect_name(e: FillEffect) -> &'static str {
    match e {
        FillEffect::Fade => "fade",
        FillEffect::Rainbow => "rainbow",
        FillEffect::AltRainbow => "alt-rainbow",
    }
}

const fn tiling_name(t: Tiling) -> &'static str {
    match t {
        Tiling::None => "none",
        Tiling::Simple => "simple",
        Tiling::Repeat => "repeat",
        Tiling::RepeatInverted => "repeat-inverted",
        Tiling::RepeatExtra => "repeat-extra",
    }
}

const fn mode_name(m: TranspMode) -> &'static str {
    match m {
        TranspMode::None => "none",
        TranspMode::Mix => "mix",
        TranspMode::StainedGlass => "stained-glass",
        TranspMode::Bleach => "bleach",
        TranspMode::Contrast => "contrast",
        TranspMode::Saturation => "saturation",
        TranspMode::Darken => "darken",
        TranspMode::Lighten => "lighten",
        TranspMode::Brightness => "brightness",
        TranspMode::Hue => "hue",
        TranspMode::Luminosity => "luminosity",
    }
}

/// Collects every fill attribute reachable from the root, in tree order.
///
/// A fill effect or fill mapping attribute is credited to the last colour
/// fill before it under the same parent, and a transparency mapping to the
/// last transparency: that is how the `.xar` format writes them ("modifies
/// the preceding fill"), and how the importer keeps them.
#[must_use]
pub fn census(doc: &Document) -> FillCensus {
    let tree = &doc.tree;
    let mut out = FillCensus::default();
    // The last fill and transparency row under each parent.
    let mut last: HashMap<(Option<NodeId>, bool), usize> = HashMap::new();
    for id in tree.preorder(tree.root()) {
        let Some(NodeKind::Attr(a)) = tree.kind(id) else {
            continue;
        };
        let parent = tree.links(id).parent;
        let in_text = tree
            .ancestors(id)
            .any(|n| matches!(tree.kind(n), Some(NodeKind::TextLine(_))));
        let fill_row = match &a.value {
            AttrValue::Fill(p) => Some(row(FillSlot::Fill, p, None)),
            AttrValue::TranspFill(t) => Some(row(FillSlot::Transparency, t, transparency_mode(t))),
            AttrValue::StrokeColour(p) => Some(row(FillSlot::Line, p, None)),
            AttrValue::StrokeTransp(t) => {
                Some(row(FillSlot::LineTransparency, t, transparency_mode(t)))
            }
            _ => None,
        };
        if let Some(mut r) = fill_row {
            r.in_text = in_text;
            match r.slot {
                FillSlot::Fill => {
                    last.insert((parent, false), out.rows.len());
                }
                FillSlot::Transparency => {
                    last.insert((parent, true), out.rows.len());
                }
                FillSlot::Line | FillSlot::LineTransparency => {}
            }
            out.rows.push(r);
            continue;
        }
        if in_text
            && matches!(
                a.value,
                AttrValue::FillEffect(_)
                    | AttrValue::FillMapping(_)
                    | AttrValue::TranspFillMapping(_)
            )
        {
            out.text_modifiers += 1;
            continue;
        }
        match &a.value {
            AttrValue::FillEffect(e) => {
                *out.effects.entry(effect_name(*e)).or_insert(0) += 1;
                if let Some(&i) = last.get(&(parent, false)) {
                    out.rows[i].effect = Some(*e);
                }
            }
            AttrValue::FillMapping(t) => {
                *out.mappings.entry(("fill", tiling_name(*t))).or_insert(0) += 1;
                if let Some(&i) = last.get(&(parent, false)) {
                    out.rows[i].tiling = Some(*t);
                }
            }
            AttrValue::TranspFillMapping(t) => {
                *out.mappings
                    .entry(("transparency", tiling_name(*t)))
                    .or_insert(0) += 1;
                if let Some(&i) = last.get(&(parent, true)) {
                    out.rows[i].tiling = Some(*t);
                }
            }
            _ => {}
        }
    }
    out
}

fn transparency_mode(t: &xarast_doc::fill::TranspPaint) -> Option<TranspMode> {
    Some(match t {
        FillGeometry::Flat { value } => value.mode,
        FillGeometry::Linear { from, .. }
        | FillGeometry::Radial { from, .. }
        | FillGeometry::Conical { from, .. }
        | FillGeometry::Diamond { from, .. }
        | FillGeometry::Fractal { from, .. }
        | FillGeometry::Noise { from, .. } => from.mode,
        FillGeometry::ThreeColour { c0, .. } | FillGeometry::FourColour { c0, .. } => c0.mode,
        FillGeometry::Bitmap { contone, .. } => contone.map_or(TranspMode::Mix, |(a, _)| a.mode),
    })
}

/// The fill-record histogram of a `.xar` file, in the census's terms:
/// `(slot, shape) → records`, plus fill effect and mapping records.
///
/// Records inside `TAG_CURRENTATTRIBUTES` (4119) are left out: they are the
/// editor's current attributes, not part of the drawing, and the importer
/// skips them (`docs/memory/xar-import.md`, finding 3). So are those inside
/// `TAG_SHADOW` (4051): a shadow's own fill and transparency become its
/// colour and darkness parameters, not attributes (finding 20); and those
/// inside `TAG_BEVELINK` (4057): copies of the bevelled object's fill, a
/// flat one becoming the bevel's colour parameter (finding 21).
///
/// # Errors
///
/// When the file cannot be parsed.
pub fn tag_census(bytes: &[u8]) -> Result<FillCensus, String> {
    let analysis = xarast_xar::analyse(bytes, xarast_xar::ReaderLimits::default())
        .map_err(|e| e.to_string())?;
    let mut out = FillCensus::default();
    let mut skip_below: Option<usize> = None;
    let mut text_below: Option<usize> = None;
    analysis.tree.walk(&mut |n, depth| {
        if let Some(d) = skip_below {
            if depth > d {
                return;
            }
            skip_below = None;
        }
        if text_below.is_some_and(|d| depth <= d) {
            text_below = None;
        }
        if n.tag() == TAG_CURRENTATTRIBUTES || n.tag() == TAG_SHADOW || n.tag() == TAG_BEVELINK {
            skip_below = Some(depth);
            return;
        }
        if n.tag() == TAG_TEXT_LINE && text_below.is_none() {
            text_below = Some(depth);
        }
        let in_text = text_below.is_some_and(|d| depth > d);
        let r = |slot, shape| FillRow {
            slot,
            shape,
            stops: 0,
            profile: BiasGain::IDENTITY,
            mapping: RampMapping::Linear,
            perspective: false,
            mode: None,
            effect: None,
            tiling: None,
            in_text,
        };
        let fill = |shape| Some(r(FillSlot::Fill, shape));
        let transp = |shape| Some(r(FillSlot::Transparency, shape));
        let row = match n.tag() {
            150 | 190..=192 => fill("flat"),
            153 | 4075 | 4121 | 4122 => fill("linear"),
            154 | 4076 => fill("circular"),
            155 | 4077 => fill("elliptical"),
            156 | 4078 => fill("conical"),
            200 | 4088 => fill("diamond"),
            202 => fill("three-colour"),
            204 => fill("four-colour"),
            157 => fill("bitmap"),
            158 => fill("contone"),
            159 => fill("fractal"),
            4010 => fill("noise"),
            166 => transp("flat"),
            167 | 4123 => transp("linear"),
            168 => transp("circular"),
            169 => transp("elliptical"),
            170 => transp("conical"),
            171 => transp("bitmap"),
            172 => transp("fractal"),
            201 => transp("diamond"),
            203 => transp("three-colour"),
            205 => transp("four-colour"),
            4011 => transp("noise"),
            151 | 193..=195 => Some(r(FillSlot::Line, "flat")),
            173 => Some(r(FillSlot::LineTransparency, "flat")),
            _ => None,
        };
        if let Some(row) = row {
            out.rows.push(row);
        }
        let effect = match n.tag() {
            160 => Some("fade"),
            161 => Some("rainbow"),
            162 => Some("alt-rainbow"),
            _ => None,
        };
        if let Some(e) = effect {
            if in_text {
                out.text_modifiers += 1;
            } else {
                *out.effects.entry(e).or_insert(0) += 1;
            }
        }
        let mapping = match n.tag() {
            163 => Some(("fill", "repeat")),
            164 => Some(("fill", "simple")),
            165 => Some(("fill", "repeat-inverted")),
            206 => Some(("fill", "repeat-extra")),
            180 => Some(("transparency", "repeat")),
            181 => Some(("transparency", "simple")),
            182 => Some(("transparency", "repeat-inverted")),
            207 => Some(("transparency", "repeat-extra")),
            _ => None,
        };
        if let Some(m) = mapping {
            if in_text {
                out.text_modifiers += 1;
            } else {
                *out.mappings.entry(m).or_insert(0) += 1;
            }
        }
    });
    Ok(out)
}

const TAG_CURRENTATTRIBUTES: u32 = 4119;
const TAG_SHADOW: u32 = 4051;
const TAG_BEVELINK: u32 = 4057;
const TAG_TEXT_LINE: u32 = 2200;

/// Opens one document and takes its census.
///
/// # Errors
///
/// A message when it cannot be read or imported.
pub fn inspect_one(path: &Path) -> Result<FillCensus, String> {
    let s = Session::open(DocumentId(1), path).map_err(|e| e.to_string())?;
    Ok(census(&s.doc))
}

/// The report for one document.
#[must_use]
pub fn report(c: &FillCensus, summary: bool) -> String {
    let mut s = String::new();
    if !summary {
        for (i, r) in c.rows.iter().enumerate() {
            let _ = write!(s, "  {i:>6}  {:<17} {:<12}", r.slot.name(), r.shape);
            if r.stops > 0 {
                let _ = write!(s, " stops={}", r.stops);
            }
            if r.profile != BiasGain::IDENTITY {
                let _ = write!(s, " profile={} {}", r.profile.bias, r.profile.gain);
            }
            if r.mapping == RampMapping::Sin {
                s.push_str(" ramp=sin");
            }
            if r.perspective {
                s.push_str(" perspective");
            }
            // A bitmap transparency always says its mode: it used to be
            // lost on import (XARA-T-0307).
            if let Some(m) = r.mode
                && (m != TranspMode::Mix || r.shape == "bitmap")
            {
                let _ = write!(s, " mode={}", mode_name(m));
            }
            if let Some(e) = r.effect {
                let _ = write!(s, " effect={}", effect_name(e));
            }
            if let Some(t) = r.tiling {
                let _ = write!(s, " mapping={}", tiling_name(t));
            }
            if r.in_text {
                s.push_str(" in-text");
            }
            s.push('\n');
        }
    }
    s.push_str("  histogram\n");
    for ((slot, shape), n) in c.histogram() {
        let _ = writeln!(s, "    {slot:<17} {shape:<12} {n:>7}");
    }
    for (e, n) in &c.effects {
        let _ = writeln!(s, "    {:<17} {e:<12} {n:>7}", "effect");
    }
    for ((side, m), n) in &c.mappings {
        let _ = writeln!(s, "    {:<17} {m:<12} {n:>7}", format!("{side}-mapping"));
    }
    s
}

/// Runs `inspect`.
#[must_use]
pub fn run(a: &InspectArgs) -> Exit {
    let files = match expand(&a.inputs) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("xarast-cli: {e}");
            return Exit::Import;
        }
    };
    let mut failed = 0usize;
    for path in &files {
        match inspect_one(path) {
            Ok(c) => {
                println!("{}: {} fills", path.display(), c.rows.len());
                print!("{}", report(&c, a.summary));
            }
            Err(e) => {
                failed += 1;
                eprintln!("FAILED: {}: {e}", path.display());
            }
        }
    }
    if failed > 0 { Exit::Import } else { Exit::Ok }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn fills_must_be_asked_for() {
        assert!(parse(&argv(&["a.xar"])).is_err());
        assert!(parse(&argv(&["--fills"])).is_err());
        let a = parse(&argv(&["--fills", "a.xar", "--summary"])).unwrap();
        assert!(a.summary);
        assert_eq!(a.inputs, vec![PathBuf::from("a.xar")]);
    }
}
