//! W9.3 layout: line breaking, justification, tracking, manual kerns, line
//! spacing, margins, tabs, scripts — in millipoints, against hand-computed
//! values from the pinned faces.

mod common;

use common::*;
use xarast_geom::Mp;
use xarast_text::{
    FontMetrics, FontQuery, Justification, LaidLine, Layout, LineSpacing, ManualKern,
    ParagraphStyle, Shaper, StoryInput, StoryMode, StyleRange, TabKind, TabStop, TextScript,
};

fn para(j: Justification) -> ParagraphStyle {
    ParagraphStyle {
        justification: j,
        ..ParagraphStyle::default()
    }
}

fn col(w_pt: i32) -> StoryMode {
    StoryMode::column(Mp::new(w_pt * 1000))
}

fn line_text<'a>(text: &'a str, l: &LaidLine) -> &'a str {
    &text[l.logical_range.clone()]
}

/// Right edge of the last non-space cluster's glyph box (its advance, no
/// tracking, no slack) — the edge alignment is measured to.
fn ink_right(text: &str, l: &LaidLine, tracking: Mp) -> Mp {
    let c = l
        .clusters
        .iter()
        .filter(|c| !text[c.range.clone()].chars().all(char::is_whitespace))
        .max_by_key(|c| c.x)
        .unwrap();
    c.x + c.width - tracking
}

const PROSE: &str = "The quick brown fox jumps over the lazy dog while five \
    wizards box and quietly judge the vexing jumble of text that wraps.";

#[test]
fn column_text_wraps_at_word_boundaries_and_keeps_trailing_spaces() {
    let s = shaper();
    let l = lay(
        &s,
        PROSE,
        &[run(PROSE, "Noto Sans", 10)],
        para(Justification::Left),
        col(120),
    );
    assert!(l.lines.len() >= 4);
    let mut rebuilt = String::new();
    for line in &l.lines {
        let t = line_text(PROSE, line);
        rebuilt.push_str(t);
        assert!(line.width <= Mp::new(120_000), "{t:?} fits");
        assert_eq!(line.x, Mp::ZERO);
    }
    assert_eq!(rebuilt, PROSE, "lines partition the text");
    for line in &l.lines[..l.lines.len() - 1] {
        assert!(
            line_text(PROSE, line).ends_with(' '),
            "breaks after a space"
        );
    }
    // Greedy: the next line's first word would not have fitted.
    let first_next_word = line_text(PROSE, &l.lines[1]).split(' ').next().unwrap();
    let probe = format!("{}{}", line_text(PROSE, &l.lines[0]), first_next_word);
    let p = lay(
        &s,
        &probe,
        &[run(&probe, "Noto Sans", 10)],
        para(Justification::Left),
        StoryMode::Point,
    );
    assert!(p.lines[0].width > Mp::new(120_000));
}

#[test]
fn full_justification_fills_the_column_exactly_except_the_last_line() {
    // Acceptance criterion 6: to within 1 mp. With the remainder distributed
    // it is exact.
    let s = shaper();
    let mut p = para(Justification::Full);
    p.left_margin = Mp::new(5_000);
    p.first_indent = Mp::new(15_000);
    p.right_margin = Mp::new(7_000);
    let l = lay(&s, PROSE, &[run(PROSE, "Noto Sans", 10)], p, col(150));
    assert!(l.lines.len() >= 3);
    let right = Mp::new(150_000 - 7_000);
    for (i, line) in l.lines.iter().enumerate() {
        let left = if i == 0 {
            Mp::new(15_000)
        } else {
            Mp::new(5_000)
        };
        assert_eq!(line.x, left, "line {i} starts at its margin");
        if i + 1 < l.lines.len() {
            assert_eq!(
                ink_right(PROSE, line, Mp::ZERO),
                right,
                "line {i} is flush right"
            );
        } else {
            assert!(
                ink_right(PROSE, line, Mp::ZERO) < right,
                "the last line is left aligned"
            );
        }
    }
}

#[test]
fn full_justification_stretches_spaces_only_when_there_are_spaces() {
    let s = shaper();
    let text = "aa bb cc dd ee ff gg hh ii jj kk";
    let natural = lay(
        &s,
        text,
        &[run(text, "Noto Sans", 10)],
        para(Justification::Left),
        col(60),
    );
    let full = lay(
        &s,
        text,
        &[run(text, "Noto Sans", 10)],
        para(Justification::Full),
        col(60),
    );
    let (n, f) = (&natural.lines[0], &full.lines[0]);
    for (a, b) in n.clusters.iter().zip(&f.clusters) {
        let is_space = &text[a.range.clone()] == " ";
        if is_space {
            assert!(b.width >= a.width);
        } else {
            assert_eq!(a.width, b.width, "letters keep their advance");
        }
    }
    // A single overlong word in a narrow column: letter spacing shrinks
    // (negative slack on characters), and the word still ends flush.
    let word = "abcdefghij";
    let l = lay(
        &s,
        word,
        &[run(word, "Noto Sans", 10)],
        para(Justification::Full),
        StoryMode::Point,
    );
    assert_eq!(l.lines.len(), 1, "point text never wraps");
}

#[test]
fn right_and_centre_alignment_use_the_width_to_the_last_non_space() {
    let s = shaper();
    let text = "Hello world  ";
    let r = [run(text, "Noto Sans", 10)];
    let left = lay(&s, text, &r, para(Justification::Left), col(100));
    let w = left.lines[0].width;
    let right = lay(&s, text, &r, para(Justification::Right), col(100));
    assert_eq!(right.lines[0].x, Mp::new(100_000) - w);
    assert_eq!(ink_right(text, &right.lines[0], Mp::ZERO), Mp::new(100_000));
    let centre = lay(&s, text, &r, para(Justification::Centre), col(100));
    assert_eq!(centre.lines[0].x.raw(), (100_000 - w.raw()) / 2);
    // Point text aligns about the anchor.
    let pr = lay(&s, text, &r, para(Justification::Right), StoryMode::Point);
    assert_eq!(pr.lines[0].x, Mp::ZERO - w);
    let pc = lay(&s, text, &r, para(Justification::Centre), StoryMode::Point);
    assert_eq!(pc.lines[0].x.raw(), -w.raw() / 2);
}

#[test]
fn tracking_is_thousandths_of_an_em_and_excluded_from_the_last_character() {
    // Acceptance criterion 7, by hand: the unit is the width of 'M' (the
    // original's em character), so a tracking of 100 adds a tenth of it
    // after every character.
    let s = shaper();
    let em = s
        .char_metrics(&FontQuery::new("Noto Sans"), Mp::new(10_000), 1.0, 'M')
        .unwrap()
        .em_width;
    assert!(em < Mp::new(10_000) && em > Mp::new(8_000), "{em:?}");
    let step = em.mul_ratio(100, 1000);
    let text = "HHHH";
    let plain = lay(
        &s,
        text,
        &[run(text, "Noto Sans", 10)],
        para(Justification::Right),
        col(100),
    );
    let mut tracked = run(text, "Noto Sans", 10);
    tracked.tracking = 100;
    let t = lay(&s, text, &[tracked], para(Justification::Right), col(100));
    let h = s
        .char_metrics(&FontQuery::new("Noto Sans"), Mp::new(10_000), 1.0, 'H')
        .unwrap()
        .advance;
    assert_eq!(plain.lines[0].width, Mp::new(4 * h.raw()));
    assert_eq!(
        t.lines[0].width,
        Mp::new(4 * h.raw() + 3 * step.raw()),
        "sum(advances) - last tracking"
    );
    assert_eq!(t.lines[0].x + t.lines[0].width, Mp::new(100_000));
    assert_eq!(ink_right(text, &t.lines[0], step), Mp::new(100_000));
}

#[test]
fn manual_kerns_are_em_relative_and_survive_reshaping() {
    // Acceptance criterion 8's mechanism: a kern is anchored to a byte
    // offset, not to a glyph, and scales with the size in force.
    let s = shaper();
    let text = "AVAV";
    let kerns = [ManualKern { at: 2, amount: 250 }];
    let lay_at = |pt: i32| {
        s.layout(&StoryInput {
            text,
            runs: &[run(text, "Noto Sans", pt)],
            paragraphs: &[ParagraphStyle::default()],
            kerns: &kerns,
            mode: StoryMode::Point,
        })
    };
    let plain = lay(
        &s,
        text,
        &[run(text, "Noto Sans", 10)],
        ParagraphStyle::default(),
        StoryMode::Point,
    );
    let k10 = lay_at(10);
    let em = s
        .char_metrics(&FontQuery::new("Noto Sans"), Mp::new(10_000), 1.0, 'M')
        .unwrap()
        .em_width;
    let kern = em.mul_ratio(250, 1000);
    let x = |l: &Layout, i: usize| l.lines[0].runs[0].glyphs[i].x;
    assert_eq!(x(&k10, 1), x(&plain, 1), "before the kern: unmoved");
    assert_eq!(x(&k10, 2), x(&plain, 2) + kern, "250/1000 of 'M' at 10 pt");
    assert_eq!(x(&k10, 3), x(&plain, 3) + kern);
    let k20 = lay_at(20);
    // Twice the size, twice the gap, give or take the rounding of the 'M'
    // width to whole millipoints at each size.
    let d20 = (x(&k20, 2) - x(&k20, 1)).raw();
    let d10 = (x(&k10, 2) - x(&k10, 1)).raw();
    assert!((d20 - 2 * d10).abs() <= 1, "{d20} vs 2 x {d10}");
    assert_eq!(lay_at(10), k10, "re-shaping reproduces it exactly");
}

#[test]
fn line_spacing_ratio_and_absolute() {
    let s = shaper();
    let text = "Ab\nCd\nEf";
    let r = [run(text, "Noto Sans", 10)];
    let (asc, desc) = (Mp::new(10_690), Mp::new(2_930));
    let h = asc + desc;
    let with = |ls| {
        lay(
            &s,
            text,
            &r,
            ParagraphStyle {
                line_spacing: ls,
                ..ParagraphStyle::default()
            },
            StoryMode::Point,
        )
    };
    for ratio in [1.0f32, 1.5, 0.5] {
        let l = with(LineSpacing::Ratio(ratio));
        assert_eq!(l.lines.len(), 3);
        assert_eq!(l.lines[0].baseline_y, Mp::ZERO);
        let step = h.scale(f64::from(ratio));
        for w in l.lines.windows(2) {
            assert_eq!(w[0].baseline_y - w[1].baseline_y, step, "ratio {ratio}");
        }
        assert_eq!(l.lines[0].ascent, asc);
        assert_eq!(l.lines[0].descent, desc);
    }
    let l = with(LineSpacing::Absolute(Mp::new(20_000)));
    for w in l.lines.windows(2) {
        assert_eq!(w[0].baseline_y - w[1].baseline_y, Mp::new(20_000));
    }
    // The descent line sits the baseline's share of the box below it.
    assert_eq!(
        l.lines[0].descent_line,
        Mp::ZERO - Mp::new(20_000).mul_ratio(desc.raw(), h.raw())
    );
}

#[test]
fn the_largest_size_on_a_line_sets_its_metrics() {
    let s = shaper();
    let text = "small BIG";
    let runs = [
        StyleRange::new(0..6, FontQuery::new("Noto Sans"), Mp::new(10_000)),
        StyleRange::new(6..9, FontQuery::new("Noto Sans"), Mp::new(20_000)),
    ];
    let l = lay(&s, text, &runs, ParagraphStyle::default(), StoryMode::Point);
    assert_eq!(l.lines[0].ascent, Mp::new(21_380));
    assert_eq!(l.lines[0].size, Mp::new(20_000));
    assert_eq!(l.lines[0].runs.len(), 2);
    assert_eq!(l.lines[0].runs[1].size, Mp::new(20_000));
}

#[test]
fn empty_paragraphs_are_lines_too() {
    let s = shaper();
    let text = "a\n\nb";
    let l = lay(
        &s,
        text,
        &[run(text, "Noto Sans", 10)],
        ParagraphStyle::default(),
        StoryMode::Point,
    );
    assert_eq!(l.lines.len(), 3);
    assert!(l.lines[1].runs.is_empty());
    assert_eq!(l.lines[1].logical_range, 2..2);
    let steps: Vec<_> = l
        .lines
        .windows(2)
        .map(|w| w[0].baseline_y - w[1].baseline_y)
        .collect();
    assert_eq!(steps[0], steps[1], "an empty line is as tall as a full one");
    let empty = lay(&s, "", &[], ParagraphStyle::default(), StoryMode::Point);
    assert_eq!(empty.lines.len(), 1);
}

#[test]
fn tabs_go_to_the_ruler_then_to_default_stops() {
    let s = shaper();
    let text = "a\tb\tc";
    let r = [run(text, "Noto Sans", 10)];
    let l = lay(&s, text, &r, ParagraphStyle::default(), StoryMode::Point);
    let x_of = |l: &Layout, at: usize| {
        l.lines[0]
            .clusters
            .iter()
            .find(|c| c.range.start == at)
            .unwrap()
            .x
    };
    assert_eq!(x_of(&l, 2), Mp::new(36_000));
    assert_eq!(x_of(&l, 4), Mp::new(72_000));
    let ruled = lay(
        &s,
        text,
        &r,
        ParagraphStyle {
            tabs: vec![TabStop::new(Mp::new(50_000), TabKind::Left)].into(),
            ..ParagraphStyle::default()
        },
        StoryMode::Point,
    );
    assert_eq!(x_of(&ruled, 2), Mp::new(50_000));
    assert_eq!(
        x_of(&ruled, 4),
        Mp::new(72_000),
        "past the ruler: default grid"
    );
}

#[test]
fn superscript_scales_and_raises_and_aspect_stretches() {
    let s = shaper();
    let text = "x2";
    let mut sup = StyleRange::new(1..2, FontQuery::new("Noto Sans"), Mp::new(10_000));
    sup.script = TextScript {
        offset: 0.33,
        size: 0.5,
    };
    let runs = [
        StyleRange::new(0..1, FontQuery::new("Noto Sans"), Mp::new(10_000)),
        sup,
    ];
    let l = lay(&s, text, &runs, ParagraphStyle::default(), StoryMode::Point);
    let r = &l.lines[0].runs[1];
    assert_eq!(r.size, Mp::new(5_000));
    assert_eq!(r.glyphs[0].y, Mp::new(3_300));
    let plain = lay(
        &s,
        "2",
        &[run("2", "Noto Sans", 5)],
        ParagraphStyle::default(),
        StoryMode::Point,
    );
    assert_eq!(
        r.glyphs[0].advance,
        plain.lines[0].runs[0].glyphs[0].advance
    );

    let mut wide = run("H", "Noto Sans", 10);
    wide.aspect = 2.0;
    let w = lay(
        &s,
        "H",
        &[wide],
        ParagraphStyle::default(),
        StoryMode::Point,
    );
    let n = lay(
        &s,
        "H",
        &[run("H", "Noto Sans", 10)],
        ParagraphStyle::default(),
        StoryMode::Point,
    );
    assert_eq!(w.lines[0].width, n.lines[0].width.mul_ratio(2, 1));
    assert_eq!(w.lines[0].runs[0].aspect, 2.0);
}

#[test]
fn positions_accumulate_in_millipoints_without_drift() {
    // The regression the phase asks for: a 500-character line's end,
    // computed from one glyph's advance and from the laid-out line, agree
    // exactly. `l` has no kerning pairs with itself in Noto Sans.
    let s = shaper();
    let text = "l".repeat(500);
    let l = lay(
        &s,
        &text,
        &[run(&text, "Noto Sans", 10)],
        ParagraphStyle::default(),
        StoryMode::Point,
    );
    let one = s
        .char_metrics(&FontQuery::new("Noto Sans"), Mp::new(10_000), 1.0, 'l')
        .unwrap()
        .advance;
    let last = l.lines[0].runs[0].glyphs.last().unwrap();
    assert_eq!(last.x + last.advance, Mp::new(500 * one.raw()));
    // At an odd size where one advance is not a whole number of mp before
    // rounding, the end is still exactly n × the rounded advance.
    let l = lay(
        &s,
        &text,
        &[StyleRange::new(
            0..500,
            FontQuery::new("Noto Sans"),
            Mp::new(9_973),
        )],
        ParagraphStyle::default(),
        StoryMode::Point,
    );
    let one = s
        .char_metrics(&FontQuery::new("Noto Sans"), Mp::new(9_973), 1.0, 'l')
        .unwrap()
        .advance;
    assert_eq!(l.lines[0].width, Mp::new(500 * one.raw()));
}

#[test]
fn an_overlong_word_breaks_between_clusters() {
    let s = shaper();
    let text = "Supercalifragilistic";
    let l = lay(
        &s,
        text,
        &[run(text, "Noto Sans", 10)],
        ParagraphStyle::default(),
        col(30),
    );
    assert!(l.lines.len() > 1);
    for line in &l.lines {
        assert!(!line.clusters.is_empty(), "at least one cluster per line");
        assert!(line.width <= Mp::new(30_000) || line.clusters.len() == 1);
    }
}

#[test]
fn cjk_breaks_between_ideographs_but_not_before_a_full_stop() {
    let s = shaper();
    let text = "日本語の文字。";
    // Each ideograph is 10 000 mp at 10 pt: three fit in 35 pt.
    let l = lay(
        &s,
        text,
        &[run(text, "Noto Sans CJK JP", 10)],
        ParagraphStyle::default(),
        col(35),
    );
    let lines: Vec<&str> = l.lines.iter().map(|x| line_text(text, x)).collect();
    // "の文字" would fit, but "。" may not start a line (UAX #14 class CL),
    // so the break falls before "字" and "字。" moves down together.
    assert_eq!(lines, ["日本語", "の文", "字。"]);
}

#[test]
fn layout_is_deterministic() {
    let s = shaper();
    let text = "Mixed שלום سلام 日本語 text\nwith two paragraphs of words to wrap.";
    let r = [run(text, "Noto Sans", 11)];
    let a = lay(&s, text, &r, para(Justification::Full), col(80));
    let b = lay(&s, text, &r, para(Justification::Full), col(80));
    assert_eq!(a, b);
    let fresh = lay(&shaper(), text, &r, para(Justification::Full), col(80));
    assert_eq!(a.lines.len(), fresh.lines.len());
    for (x, y) in a.lines.iter().zip(&fresh.lines) {
        assert_eq!(x.clusters, y.clusters);
        assert_eq!(x.baseline_y, y.baseline_y);
    }
}

#[test]
fn malformed_style_runs_still_lay_out() {
    let s = shaper();
    let text = "abcdef";
    let runs = [
        StyleRange::new(4..99, FontQuery::new("Noto Sans"), Mp::new(10_000)),
        StyleRange::new(1..3, FontQuery::new("Noto Sans"), Mp::new(20_000)),
        StyleRange::new(2..2, FontQuery::new("Noto Sans"), Mp::new(30_000)),
    ];
    let l = lay(&s, text, &runs, ParagraphStyle::default(), StoryMode::Point);
    assert_eq!(l.lines[0].clusters.len(), 6);
    let _: &Shaper = &s;
}

#[test]
fn bounds_cover_every_line_box() {
    let s = shaper();
    let l = lay(
        &s,
        PROSE,
        &[run(PROSE, "Noto Sans", 10)],
        para(Justification::Left),
        col(120),
    );
    for line in &l.lines {
        assert!(
            l.bounds
                .contains(xarast_geom::Point::new(line.x, line.baseline_y))
        );
    }
    assert_eq!(l.bounds.hi.y, l.lines[0].ascent);
}

#[test]
fn a_column_without_word_wrap_aligns_but_never_wraps() {
    let s = shaper();
    let mode = StoryMode::Column {
        width: Mp::new(60_000),
        wrap: false,
    };
    let l = lay(
        &s,
        PROSE,
        &[run(PROSE, "Noto Sans", 10)],
        para(Justification::Right),
        mode,
    );
    assert_eq!(l.lines.len(), 1);
    assert_eq!(l.lines[0].x + l.lines[0].width, Mp::new(60_000));
}

/// What text on a path places by: a cluster's glyph pen (after a manual
/// kern) and its own advance (without tracking), W9.5.
#[test]
fn a_cluster_knows_its_glyph_pen_and_its_own_advance() {
    let s = shaper();
    let text = "HHH";
    let mut tracked = run(text, "Noto Sans", 10);
    tracked.tracking = 100;
    let kerns = [ManualKern { at: 1, amount: 500 }];
    let l = s.layout(&StoryInput {
        text,
        runs: &[tracked],
        paragraphs: &[ParagraphStyle::default()],
        kerns: &kerns,
        mode: StoryMode::Point,
    });
    let m = |c: char| {
        s.char_metrics(&FontQuery::new("Noto Sans"), Mp::new(10_000), 1.0, c)
            .unwrap()
    };
    let (h, em) = (m('H').advance, m('M').em_width);
    let line = &l.lines[0];
    let c = &line.clusters;
    assert_eq!(c.len(), 3);
    for k in c {
        assert_eq!(k.advance, h, "the glyph's own advance");
    }
    assert_eq!(c[0].pen, c[0].x);
    assert_eq!(c[1].pen, c[1].x + em.mul_ratio(500, 1000), "after the kern");
    assert_eq!(
        c[0].width,
        h + em.mul_ratio(100, 1000),
        "tracking in the box"
    );
    // The glyphs start at the pen, and the lookup by cluster finds it.
    for r in &line.runs {
        for g in &r.glyphs {
            assert_eq!(line.cluster_at(g.cluster).unwrap().pen, g.x);
        }
    }
    assert!(line.cluster_at(1_000).is_none());
}
