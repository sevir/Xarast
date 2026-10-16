//! Line breaking, justification, line metrics and bidi reordering: shaped
//! paragraphs in, positioned lines out.
//!
//! The rules follow the behaviour of the original's line formatter, recorded
//! as facts in `docs/memory/text.md` (with `file:line` references); the code
//! is ours.
//!
//! # Coordinates
//!
//! Story space, in millipoints, **y up** like the rest of the document: the
//! first line's baseline is `y = 0`, later lines have negative baselines,
//! `x = 0` is the story's left edge (the column's left edge in column mode,
//! the anchor point in point mode).

use std::ops::Range;
use std::sync::Arc;

use unicode_bidi::{Level, ParagraphBidiInfo};
use xarast_geom::{Mp, Point, Rect};

use crate::font::{FaceId, FontSubstitution};
use crate::shape::{
    ClusterKind, ParagraphRequest, ShapedParagraph, Shaper, face_metrics, normalise_runs,
    resolve_families, shape_paragraph,
};
use crate::style::{
    Direction, Justification, LineSpacing, ParagraphStyle, StoryInput, StoryMode, StyleRange,
    TabKind, TabStop, sanitize_aspect,
};

/// Distance between default tab stops when the ruler has none left: half an
/// inch (`Kernel/txtattr.cpp:3203-3204`).
pub const DEFAULT_TAB_INTERVAL: Mp = Mp::new(36_000);

/// One glyph, placed.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct PlacedGlyph {
    /// Glyph id in the run's face.
    pub id: u32,
    /// Pen position plus the glyph's own offset, story space.
    pub x: Mp,
    /// Baseline plus shifts plus the glyph's own offset, story space, y up.
    pub y: Mp,
    /// The glyph's shaped advance.
    pub advance: Mp,
    /// Byte offset of the first character of the cluster it belongs to.
    pub cluster: usize,
}

/// A visual run of glyphs sharing a face, size and style.
#[derive(Clone, PartialEq, Debug)]
pub struct GlyphRun {
    /// The face.
    pub face: FaceId,
    /// The size glyphs are drawn at (super/subscript applied).
    pub size: Mp,
    /// Horizontal stretch.
    pub aspect: f32,
    /// Normalised variation coordinates (F2Dot14 bits), for outlines.
    pub coords: Arc<[i16]>,
    /// Index of the input style run this comes from.
    pub style: usize,
    /// The bidi embedding level; odd is right to left.
    pub level: u8,
    /// Glyphs, left to right.
    pub glyphs: Vec<PlacedGlyph>,
}

impl GlyphRun {
    /// The transform from this run's glyph outlines (font units, y up) to
    /// story space for glyph `g`: scale by `size / units_per_em` (stretched
    /// by the aspect ratio), then translate to the glyph's position.
    #[must_use]
    pub fn glyph_transform(&self, g: &PlacedGlyph, units_per_em: u16) -> kurbo::Affine {
        let k = self.size.to_f64() / f64::from(units_per_em.max(1));
        kurbo::Affine::new([
            k * f64::from(self.aspect),
            0.0,
            0.0,
            k,
            g.x.to_f64(),
            g.y.to_f64(),
        ])
    }
}

/// One cluster's box on a line, for hit-testing, carets and selection.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LaidCluster {
    /// Byte range in the story text.
    pub range: Range<usize>,
    /// Left edge of the cluster's box.
    pub x: Mp,
    /// Width of the box: advance, tracking, kerning and justification slack.
    pub width: Mp,
    /// Where the cluster's glyphs start: the box's left edge plus whatever
    /// the layout put before them (a manual kern, or tracking and slack in
    /// right-to-left text). The original's "position in line".
    pub pen: Mp,
    /// The glyphs' own advance, without tracking, kerning or slack (the
    /// original's character width). Zero for a tab.
    pub advance: Mp,
    /// Right to left.
    pub rtl: bool,
}

/// One formatted line.
#[derive(Clone, PartialEq, Debug)]
pub struct LaidLine {
    /// Byte range of the line's text (trailing spaces included, the
    /// paragraph's `'\n'` excluded).
    pub logical_range: Range<usize>,
    /// Paragraph index.
    pub paragraph: usize,
    /// The paragraph's last line.
    pub ends_paragraph: bool,
    /// Baseline y.
    pub baseline_y: Mp,
    /// The bottom of the line box: where the next line's box starts.
    pub descent_line: Mp,
    /// Largest ascent on the line.
    pub ascent: Mp,
    /// Largest descent on the line, positive.
    pub descent: Mp,
    /// Largest font size on the line.
    pub size: Mp,
    /// The paragraph's base direction is right to left (UAX #9 P2-P3, or
    /// forced by [`ParagraphStyle::base_direction`]): which end of the line
    /// is its logical start, for carets and line-end navigation.
    pub base_rtl: bool,
    /// x of the line's first (leftmost) cluster.
    pub x: Mp,
    /// Width used for alignment: the advances up to the last non-space
    /// character, excluding that character's tracking.
    pub width: Mp,
    /// Glyph runs, left to right.
    pub runs: Vec<GlyphRun>,
    /// Clusters in **logical** order.
    pub clusters: Vec<LaidCluster>,
}

impl LaidLine {
    /// The cluster starting at byte `start` (a [`PlacedGlyph::cluster`]).
    #[must_use]
    pub fn cluster_at(&self, start: usize) -> Option<&LaidCluster> {
        self.clusters
            .binary_search_by_key(&start, |c| c.range.start)
            .ok()
            .and_then(|i| self.clusters.get(i))
    }
}

/// The result of laying out one story.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Layout {
    /// Lines, top to bottom.
    pub lines: Vec<LaidLine>,
    /// Union of the line boxes (advance boxes, not ink).
    pub bounds: Rect,
    /// Families substituted while laying out.
    pub substitutions: Vec<FontSubstitution>,
}

impl Layout {
    /// Every placed glyph, in line then visual order, with its run.
    pub fn glyphs(&self) -> impl Iterator<Item = (&GlyphRun, &PlacedGlyph)> + '_ {
        self.lines
            .iter()
            .flat_map(|l| l.runs.iter())
            .flat_map(|r| r.glyphs.iter().map(move |g| (r, g)))
    }
}

/// A line under construction: clusters `start..end` of the shaped paragraph.
struct LineSpan {
    start: usize,
    end: usize,
    first_of_paragraph: bool,
    last_of_paragraph: bool,
    /// Resolved advance of each tab cluster in the span.
    tab_advances: Vec<(usize, Mp)>,
}

/// The first stop strictly beyond `pos`: its position, kind and decimal
/// character. Past the last ruler stop the stops are left stops on the
/// default grid (`Kernel/txtattr.cpp:3181-3205`).
fn next_tab_stop(pos: Mp, para: &ParagraphStyle) -> (Mp, TabKind, char) {
    for t in para.tabs.iter() {
        if t.pos > pos {
            return (t.pos, t.kind, t.decimal);
        }
    }
    let p = i64::from(pos.raw());
    let step = i64::from(DEFAULT_TAB_INTERVAL.raw());
    let next = p + step - p.rem_euclid(step);
    (
        Mp::new(i32::try_from(next).unwrap_or(i32::MAX)),
        TabKind::Left,
        TabStop::DEFAULT_DECIMAL,
    )
}

/// The state of the tab section being measured: the text since the last
/// tab, anchored to that tab's stop (`Kernel/nodetxtl.cpp`, `FormatState`).
///
/// A left stop starts its text at the stop. For the other kinds, `remaining`
/// is the room between the tab character and the stop; part of the text
/// (half for centre, all for right, up to the decimal point for decimal)
/// goes into that room, and only what does not fit pushes `width` past the
/// stop. The tab's own width is whatever room is left when the section
/// ends.
#[derive(Copy, Clone)]
struct FormatState {
    /// The right edge of everything placed so far.
    width: Mp,
    kind: TabKind,
    remaining: Mp,
    decimal: char,
    decimal_found: bool,
}

impl FormatState {
    fn new(indent: Mp) -> FormatState {
        FormatState {
            width: indent,
            kind: TabKind::Left,
            remaining: Mp::ZERO,
            decimal: TabStop::DEFAULT_DECIMAL,
            decimal_found: false,
        }
    }

    /// Places a character of width `w`. `is_point` is true for the decimal
    /// character of an active decimal section.
    fn advance_by(&mut self, w: Mp, is_point: bool) {
        let left_of_stop = self.remaining > Mp::ZERO;
        match self.kind {
            TabKind::Centre if left_of_stop => {
                let half = Mp::new(w.raw() / 2);
                let used = if self.remaining >= half {
                    half
                } else {
                    self.remaining
                };
                self.remaining = self.remaining - used;
                self.width = add(self.width, w - used);
            }
            TabKind::Right | TabKind::Decimal
                if left_of_stop
                    && (self.kind == TabKind::Right || (!self.decimal_found && !is_point)) =>
            {
                if self.remaining >= w {
                    self.remaining = self.remaining - w;
                } else {
                    self.width = add(self.width, w - self.remaining);
                    self.remaining = Mp::ZERO;
                }
            }
            _ => self.width = add(self.width, w),
        }
    }

    /// Whether a character of width `w` still fits within `limit`. For a
    /// left section that is the plain sum; otherwise part of the character
    /// goes left of the stop, so the answer is found by placing it.
    fn fits(&self, w: Mp, is_point: bool, limit: Mp) -> bool {
        let mut t = *self;
        t.advance_by(w, is_point);
        t.width <= limit
    }
}

fn add(a: Mp, b: Mp) -> Mp {
    a.saturating_add(b)
}

/// Splits a shaped paragraph into lines.
fn break_lines(
    text: &str,
    sp: &ShapedParagraph,
    para: &ParagraphStyle,
    mode: StoryMode,
) -> Vec<LineSpan> {
    let n = sp.clusters.len();
    let wrap_limit = match mode {
        StoryMode::Column { width, wrap: true } if width > Mp::ZERO => {
            Some(width - para.right_margin)
        }
        _ => None,
    };
    let mut lines = Vec::new();
    let mut start = 0usize;
    loop {
        let first = lines.is_empty();
        let left = if first {
            para.first_indent
        } else {
            para.left_margin
        };
        let mut st = FormatState::new(left);
        let mut last_break: Option<usize> = None;
        // Each tab: its cluster, whether it is a left tab and the advance a
        // left tab takes.
        let mut tab_marks: Vec<(usize, bool, Mp)> = Vec::new();
        // Room left of the active stop after each cluster, from `start`: a
        // non-left tab's advance is what remains when its section ends.
        let mut remaining_after: Vec<Mp> = Vec::new();
        let mut end = n;
        let mut i = start;
        while i < n {
            let c = &sp.clusters[i];
            match c.kind {
                ClusterKind::Space => {
                    st.advance_by(add(c.kern_before, add(c.advance, c.tracking)), false);
                }
                ClusterKind::Tab => {
                    // A line may break before a tab.
                    if i > start {
                        last_break = Some(i - 1);
                    }
                    let (stop, kind, decimal) = next_tab_stop(add(st.width, c.kern_before), para);
                    let adv = stop - st.width;
                    tab_marks.push((i, kind == TabKind::Left, adv));
                    st.width = stop;
                    st.kind = kind;
                    st.remaining = if kind == TabKind::Left { Mp::ZERO } else { adv };
                    st.decimal = decimal;
                    st.decimal_found = false;
                }
                ClusterKind::Char => {
                    let is_point = st.kind == TabKind::Decimal
                        && text
                            .get(c.range.clone())
                            .and_then(|t| t.chars().next())
                            .is_some_and(|ch| ch == st.decimal);
                    let w = add(c.kern_before, c.advance);
                    let fits = wrap_limit.is_none_or(|lim| st.fits(w, is_point, lim));
                    if i > start && !fits {
                        end = match last_break {
                            Some(b) if b + 1 > start => b + 1,
                            _ => i, // emergency break before the overflowing cluster
                        };
                        break;
                    }
                    st.advance_by(add(w, c.tracking), is_point);
                    if is_point {
                        st.decimal_found = true;
                    }
                }
            }
            remaining_after.push(st.remaining);
            if c.break_after {
                last_break = Some(i);
            }
            i += 1;
        }
        let tabs = tab_marks
            .iter()
            .enumerate()
            .filter(|&(_, &(t, _, _))| t < end)
            .map(|(k, &(t, left, adv))| {
                if left {
                    return (t, adv);
                }
                // The section runs to the next tab or the end of the line.
                let section_end = tab_marks
                    .get(k + 1)
                    .map_or(end, |&(nt, _, _)| nt.min(end))
                    .saturating_sub(1);
                let rem = remaining_after
                    .get(section_end.saturating_sub(start))
                    .copied()
                    .unwrap_or(Mp::ZERO);
                (t, rem)
            })
            .collect();
        let done = end >= n;
        lines.push(LineSpan {
            start,
            end,
            first_of_paragraph: first,
            last_of_paragraph: done,
            tab_advances: tabs,
        });
        if done {
            break;
        }
        start = end;
    }
    lines
}

/// Extra space given to each cluster by full justification.
struct Slack {
    /// Per cluster in the span (indexed from `span.start`).
    extra: Vec<Mp>,
}

/// Distributes `gap` over `n` recipients: every one gets the truncated
/// share, and the remainder goes one millipoint at a time to the first ones,
/// so the total is exact. The original truncates and drops the remainder
/// (`Kernel/nodetxtl.cpp:1557-1569`); distributing it keeps a fully justified
/// line exactly as wide as its column, at a cost of at most 1 mp per gap.
fn shares(gap: i64, n: i64) -> impl Iterator<Item = Mp> {
    let base = gap / n;
    let rem = gap - base * n;
    let step = rem.signum();
    let mut left = rem.abs();
    (0..n).map(move |_| {
        let extra = if left > 0 {
            left -= 1;
            step
        } else {
            0
        };
        Mp::new(i32::try_from(base + extra).unwrap_or(0))
    })
}

impl Shaper {
    /// Lays out a whole story.
    #[must_use]
    pub fn layout(&self, input: &StoryInput<'_>) -> Layout {
        let mut st = self.lock();
        let mut db = self.fonts.lock();
        let text = input.text;
        let runs = normalise_runs(input.runs, text.len());
        let mut out = Layout::default();
        let families = resolve_families(&mut db, &runs, &mut out.substitutions);

        let default_para = ParagraphStyle::default();
        let para_style = |i: usize| -> &ParagraphStyle {
            input
                .paragraphs
                .get(i)
                .or_else(|| input.paragraphs.last())
                .unwrap_or(&default_para)
        };

        let mut prev_descent_line: Option<Mp> = None;
        let mut bounds: Option<Rect> = None;
        let mut start = 0usize;
        for (pi, piece) in text.split('\n').enumerate() {
            let range = start..start + piece.len();
            start = range.end + 1;
            let para = para_style(pi);
            let sp = shape_paragraph(
                &mut st,
                &mut db,
                &ParagraphRequest {
                    text,
                    range: range.clone(),
                    runs: &runs,
                    families: &families,
                    kerns: input.kerns,
                    auto_kern: para.auto_kern,
                    direction: para.base_direction,
                },
            );
            let bidi = ParagraphBidiInfo::new(
                piece,
                match para.base_direction {
                    Direction::Auto => None,
                    Direction::Ltr => Some(Level::ltr()),
                    Direction::Rtl => Some(Level::rtl()),
                },
            );
            for span in break_lines(text, &sp, para, input.mode) {
                let line = self.position_line(
                    &mut st,
                    &mut db,
                    &sp,
                    &span,
                    para,
                    input.mode,
                    &runs,
                    &bidi,
                    pi,
                    &mut prev_descent_line,
                );
                let lb = Rect::new(
                    Point::new(line.x, line.descent_line),
                    Point::new(add(line.x, line.width), add(line.baseline_y, line.ascent)),
                );
                bounds = Some(bounds.map_or(lb, |b| b.union(lb)));
                out.lines.push(line);
            }
        }
        out.bounds = bounds.unwrap_or(Rect::EMPTY);
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn position_line(
        &self,
        st: &mut crate::shape::ShaperState,
        db: &mut crate::font::DbInner,
        sp: &ShapedParagraph,
        span: &LineSpan,
        para: &ParagraphStyle,
        mode: StoryMode,
        runs: &[StyleRange],
        bidi: &ParagraphBidiInfo<'_>,
        paragraph: usize,
        prev_descent_line: &mut Option<Mp>,
    ) -> LaidLine {
        let clusters = sp.clusters.get(span.start..span.end).unwrap_or(&[]);
        let tab_adv = |i: usize| {
            span.tab_advances
                .iter()
                .find(|&&(t, _)| t == i)
                .map_or(Mp::ZERO, |&(_, a)| a)
        };
        let style_of = |run: u32| -> Option<&StyleRange> {
            sp.runs
                .get(run as usize)
                .and_then(|r| runs.get(r.style as usize))
        };

        // ── Line metrics ────────────────────────────────────────────────
        let mut ascent = Mp::ZERO;
        let mut descent = Mp::ZERO;
        let mut size = Mp::ZERO;
        let mut measured = false;
        for c in clusters {
            let Some(r) = sp.runs.get(c.run as usize) else {
                continue;
            };
            let Some(s) = runs.get(r.style as usize) else {
                continue;
            };
            let sz = s.effective_size();
            if let Some(m) = face_metrics(st, db, r.face, &r.coords) {
                let m = m.scaled(sz);
                ascent = ascent.max(m.ascent);
                descent = descent.max(m.descent);
            }
            size = size.max(sz);
            measured = true;
        }
        if !measured {
            // An empty line takes its metrics from the style in force at its
            // position.
            let at = sp.range.start;
            let s = runs
                .iter()
                .find(|r| r.range.start <= at && at < r.range.end.max(r.range.start + 1))
                .or_else(|| runs.iter().rev().find(|r| r.range.start <= at))
                .or_else(|| runs.first());
            if let Some(s) = s {
                let q = s.font.clone();
                if let Some(m) = db.query(&q, s.panose)
                    && let Some(fm) = face_metrics(st, db, m.face, &Arc::from(Vec::new()))
                {
                    let sm = fm.scaled(s.effective_size());
                    ascent = sm.ascent;
                    descent = sm.descent;
                }
                size = s.effective_size();
            }
        }

        // ── Baseline and descent line (research/02 §7.3, CalcBaseAndDescentLine) ──
        let height = add(ascent, descent);
        let spacing = match para.line_spacing {
            LineSpacing::Absolute(s) if s != Mp::ZERO => LineSpacing::Absolute(s),
            LineSpacing::Absolute(_) => LineSpacing::Ratio(1.0),
            r => r,
        };
        let (baseline_y, descent_line) = match spacing {
            LineSpacing::Absolute(s) => {
                let off = if height > Mp::ZERO {
                    s.mul_ratio(descent.raw(), height.raw())
                } else {
                    Mp::ZERO
                };
                match *prev_descent_line {
                    Some(last) => {
                        let dl = last - s;
                        (add(dl, off), dl)
                    }
                    None => (Mp::ZERO, Mp::ZERO - off),
                }
            }
            LineSpacing::Ratio(r) => {
                let r = if r.is_finite() && r >= 0.0 {
                    f64::from(r)
                } else {
                    1.0
                };
                let abs = height.scale(r);
                let to_base = if r < 1.0 { ascent.scale(r) } else { ascent };
                match *prev_descent_line {
                    Some(last) => (last - to_base, last - abs),
                    None => (Mp::ZERO, add(to_base, Mp::ZERO) - abs),
                }
            }
        };
        *prev_descent_line = Some(descent_line);

        // ── Horizontal extent and justification counts ─────────────────
        let last_tab = clusters.iter().rposition(|c| c.kind == ClusterKind::Tab);
        let stretch_from = last_tab.map_or(0, |t| t + 1);
        let mut sum = Mp::ZERO; // running sum of full advances
        let mut sum_to_last = Mp::ZERO;
        let mut last_non_space: Option<usize> = None;
        for (i, c) in clusters.iter().enumerate() {
            let full = match c.kind {
                ClusterKind::Tab => tab_adv(span.start + i),
                _ => add(c.kern_before, add(c.advance, c.tracking)),
            };
            if c.kind != ClusterKind::Space {
                last_non_space = Some(i);
                sum_to_last = match c.kind {
                    ClusterKind::Tab => add(sum, full),
                    _ => add(sum, add(c.kern_before, c.advance)),
                };
            }
            sum = add(sum, full);
        }
        let (mut n_chars, mut n_spaces) = (0i64, 0i64);
        if let Some(last) = last_non_space {
            for c in clusters.get(stretch_from..=last).unwrap_or(&[]) {
                match c.kind {
                    ClusterKind::Char => n_chars += 1,
                    ClusterKind::Space => {
                        n_chars += 1;
                        n_spaces += 1;
                    }
                    ClusterKind::Tab => {}
                }
            }
        }

        let (phys_right, wrapping) = match mode {
            StoryMode::Column { width, wrap } if width > Mp::ZERO => (width, wrap),
            _ => (Mp::ZERO, false),
        };
        let para_left = if span.first_of_paragraph {
            para.first_indent
        } else {
            para.left_margin
        };
        let left = para_left;
        let right = phys_right - para.right_margin;
        let mut just = para.justification;
        if wrapping
            && just == Justification::Full
            && sum_to_last < right - left
            && span.last_of_paragraph
        {
            just = Justification::Left;
        }
        let x_start = match just {
            Justification::Left | Justification::Full => left,
            Justification::Right => right - sum_to_last,
            Justification::Centre => {
                let v = (i64::from(left.raw()) + i64::from(right.raw())
                    - i64::from(sum_to_last.raw()))
                    / 2;
                Mp::new(i32::try_from(v).unwrap_or(0))
            }
        };
        let mut slack = Slack {
            extra: vec![Mp::ZERO; clusters.len()],
        };
        if just == Justification::Full
            && right - left != Mp::ZERO
            && let Some(last) = last_non_space
        {
            let gap = i64::from((right - left).raw()) - i64::from(sum_to_last.raw());
            let region = stretch_from..=last;
            if gap > 0 && n_spaces > 0 {
                let mut s = shares(gap, n_spaces);
                for i in region {
                    if clusters[i].kind == ClusterKind::Space {
                        slack.extra[i] = s.next().unwrap_or(Mp::ZERO);
                    }
                }
            } else if (gap > 0 && wrapping || gap < 0) && n_chars > 1 {
                let mut s = shares(gap, n_chars - 1);
                // Every stretchable cluster but the last gets a share.
                for i in region.clone() {
                    if i == last {
                        break;
                    }
                    if clusters[i].kind != ClusterKind::Tab {
                        slack.extra[i] = s.next().unwrap_or(Mp::ZERO);
                    }
                }
            }
        }

        // ── Visual order ────────────────────────────────────────────────
        let line_bytes = match (clusters.first(), clusters.last()) {
            (Some(f), Some(l)) => (f.range.start - sp.range.start)..(l.range.end - sp.range.start),
            _ => 0..0,
        };
        let (levels, vruns) = if line_bytes.is_empty() {
            (Vec::new(), Vec::new())
        } else {
            bidi.visual_runs(line_bytes.clone())
        };
        let level_at =
            |c: usize| -> u8 { levels.get(c - sp.range.start).map_or(0, |l| l.number()) };
        let mut order: Vec<usize> = Vec::with_capacity(clusters.len());
        for vr in &vruns {
            let lo = vr.start + sp.range.start;
            let hi = vr.end + sp.range.start;
            let mut idx: Vec<usize> = (0..clusters.len())
                .filter(|&i| clusters[i].range.start >= lo && clusters[i].range.start < hi)
                .collect();
            if level_at(lo) % 2 == 1 {
                idx.reverse();
            }
            order.extend(idx);
        }

        // ── Positions ───────────────────────────────────────────────────
        let mut pen = x_start;
        let mut laid = vec![
            LaidCluster {
                range: 0..0,
                x: Mp::ZERO,
                width: Mp::ZERO,
                pen: Mp::ZERO,
                advance: Mp::ZERO,
                rtl: false
            };
            clusters.len()
        ];
        let mut out_runs: Vec<GlyphRun> = Vec::new();
        for &i in &order {
            let c = &clusters[i];
            let level = level_at(c.range.start);
            let rtl = level % 2 == 1;
            let box_start = pen;
            let (pre, post) = match c.kind {
                ClusterKind::Tab => (Mp::ZERO, add(tab_adv(span.start + i), slack.extra[i])),
                _ if rtl => (add(c.tracking, slack.extra[i]), c.kern_before),
                _ => (c.kern_before, add(c.tracking, slack.extra[i])),
            };
            pen = add(pen, pre);
            let glyph_pen = pen;
            if c.kind != ClusterKind::Tab {
                pen = add(pen, c.advance);
            }
            pen = add(pen, post);
            laid[i] = LaidCluster {
                range: c.range.clone(),
                x: box_start,
                width: pen - box_start,
                pen: glyph_pen,
                advance: if c.kind == ClusterKind::Tab {
                    Mp::ZERO
                } else {
                    c.advance
                },
                rtl,
            };
            if c.kind == ClusterKind::Tab {
                continue;
            }
            let Some(run) = sp.runs.get(c.run as usize) else {
                continue;
            };
            let style = style_of(c.run);
            let size = style.map_or(Mp::new(12_000), StyleRange::effective_size);
            let shift = style.map_or(Mp::ZERO, |s| add(s.baseline_shift, s.script.shift(s.size)));
            let aspect = style.map_or(1.0, |s| sanitize_aspect(s.aspect));
            let need_new = out_runs.last().is_none_or(|r| {
                r.face != run.face
                    || r.style != run.style as usize
                    || r.level != level
                    || r.coords != run.coords
            });
            if need_new {
                out_runs.push(GlyphRun {
                    face: run.face,
                    size,
                    aspect,
                    coords: run.coords.clone(),
                    style: run.style as usize,
                    level,
                    glyphs: Vec::new(),
                });
            }
            let Some(gr) = out_runs.last_mut() else {
                continue;
            };
            for g in sp
                .glyphs
                .get(c.glyphs.start as usize..c.glyphs.end as usize)
                .unwrap_or(&[])
            {
                gr.glyphs.push(PlacedGlyph {
                    id: g.id,
                    x: add(glyph_pen, g.x),
                    y: add(add(baseline_y, shift), g.y),
                    advance: g.advance,
                    cluster: c.range.start,
                });
            }
        }

        let logical_range = match (clusters.first(), clusters.last()) {
            (Some(f), Some(l)) => f.range.start..l.range.end,
            _ => sp.range.start..sp.range.start,
        };
        LaidLine {
            logical_range,
            paragraph,
            ends_paragraph: span.last_of_paragraph,
            baseline_y,
            descent_line,
            ascent,
            descent,
            size,
            base_rtl: bidi.paragraph_level.is_rtl(),
            x: x_start,
            width: sum_to_last,
            runs: out_runs,
            clusters: laid,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shares_are_exact_and_even() {
        let v: Vec<i32> = shares(10, 4).map(Mp::raw).collect();
        assert_eq!(v, [3, 3, 2, 2]);
        assert_eq!(v.iter().sum::<i32>(), 10);
        let v: Vec<i32> = shares(-10, 4).map(Mp::raw).collect();
        assert_eq!(v, [-3, -3, -2, -2]);
        let v: Vec<i32> = shares(7, 7).map(Mp::raw).collect();
        assert_eq!(v, [1; 7]);
    }

    #[test]
    fn default_tab_stops_are_every_half_inch_strictly_after() {
        let p = ParagraphStyle::default();
        assert_eq!(next_tab_stop(Mp::ZERO, &p).0, Mp::new(36_000));
        assert_eq!(next_tab_stop(Mp::new(36_000), &p).0, Mp::new(72_000));
        assert_eq!(next_tab_stop(Mp::new(1), &p).0, Mp::new(36_000));
    }
}
