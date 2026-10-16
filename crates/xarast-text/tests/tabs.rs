//! Right, centre and decimal tab stops (T9.3.8): where the text that
//! follows a tab lands, what a section that overflows its stop does, and a
//! decimal stop whose text has no decimal character.

mod common;

use common::*;
use xarast_geom::Mp;
use xarast_text::{Layout, ParagraphStyle, StoryMode, TabKind, TabStop};

const STOP: i32 = 100_000;

fn ruled(stops: &[TabStop]) -> ParagraphStyle {
    ParagraphStyle {
        tabs: stops.to_vec().into(),
        ..ParagraphStyle::default()
    }
}

fn one(kind: TabKind) -> ParagraphStyle {
    ruled(&[TabStop::new(Mp::new(STOP), kind)])
}

fn layout(text: &str, para: ParagraphStyle, mode: StoryMode) -> Layout {
    let s = shaper();
    lay(&s, text, &[run(text, "Noto Sans", 10)], para, mode)
}

/// `(x, right edge)` of the cluster that starts at byte `at`.
fn span(l: &Layout, line: usize, at: usize) -> (Mp, Mp) {
    let c = l.lines[line]
        .clusters
        .iter()
        .find(|c| c.range.start == at)
        .unwrap();
    (c.x, c.x + c.width)
}

/// The width of `word` alone, with no tabs.
fn width_of(word: &str) -> Mp {
    layout(word, ParagraphStyle::default(), StoryMode::Point).lines[0].width
}

#[test]
fn a_left_stop_still_starts_the_text_at_the_stop() {
    let l = layout("a\tWORD", one(TabKind::Left), StoryMode::Point);
    assert_eq!(span(&l, 0, 2).0, Mp::new(STOP));
}

#[test]
fn a_right_stop_ends_the_text_at_the_stop() {
    let l = layout("a\tWORD", one(TabKind::Right), StoryMode::Point);
    let w = width_of("WORD");
    assert_eq!(span(&l, 0, 2).0, Mp::new(STOP) - w);
    assert_eq!(
        span(&l, 0, 5).1,
        Mp::new(STOP),
        "the last letter ends there"
    );
    assert_eq!(l.lines[0].width, Mp::new(STOP), "the line ends at the stop");
}

#[test]
fn a_centre_stop_puts_half_the_text_either_side() {
    let l = layout("a\tWORD", one(TabKind::Centre), StoryMode::Point);
    let w = width_of("WORD");
    // Half is the truncated half of each character, as in the original, so
    // allow a millipoint per character.
    let left = span(&l, 0, 2).0;
    let expect = Mp::new(STOP - w.raw() / 2);
    assert!(
        (left.raw() - expect.raw()).abs() <= 4,
        "{left:?} vs {expect:?}"
    );
    let right = l.lines[0].width;
    let expect = Mp::new(STOP + w.raw() - w.raw() / 2);
    assert!(
        (right.raw() - expect.raw()).abs() <= 4,
        "{right:?} vs {expect:?}"
    );
}

#[test]
fn a_decimal_stop_puts_its_decimal_point_at_the_stop() {
    let l = layout("a\t12.50", one(TabKind::Decimal), StoryMode::Point);
    assert_eq!(
        span(&l, 0, 4).0,
        Mp::new(STOP),
        "the point starts at the stop"
    );
    assert_eq!(span(&l, 0, 2).0, Mp::new(STOP) - width_of("12"));
    assert_eq!(span(&l, 0, 5).0, Mp::new(STOP) + width_of("."));
}

#[test]
fn a_decimal_stop_honours_its_own_character() {
    let mut st = TabStop::new(Mp::new(STOP), TabKind::Decimal);
    st.decimal = ',';
    let l = layout("a\t12,50", ruled(&[st]), StoryMode::Point);
    assert_eq!(span(&l, 0, 4).0, Mp::new(STOP));
    // The full stop is then an ordinary character: only the comma counts.
    let l = layout("a\t1.2,5", ruled(&[st]), StoryMode::Point);
    assert_eq!(span(&l, 0, 5).0, Mp::new(STOP), "the comma is the point");
}

#[test]
fn a_decimal_stop_without_a_decimal_point_aligns_right() {
    let text = "a\t1250";
    let dec = layout(text, one(TabKind::Decimal), StoryMode::Point);
    let right = layout(text, one(TabKind::Right), StoryMode::Point);
    assert_eq!(dec.lines[0].clusters, right.lines[0].clusters);
    assert_eq!(span(&dec, 0, 5).1, Mp::new(STOP));
}

#[test]
fn text_after_the_decimal_point_goes_right_of_the_stop() {
    let l = layout("\t1.2345678", one(TabKind::Decimal), StoryMode::Point);
    assert_eq!(span(&l, 0, 2).0, Mp::new(STOP));
    assert_eq!(l.lines[0].width, Mp::new(STOP) + width_of(".2345678"));
}

#[test]
fn a_right_section_wider_than_the_room_pushes_past_the_stop() {
    let word = "WIDEWORDWIDEWORDWIDEWORD";
    let text = format!("ab\t{word}");
    let l = layout(&text, one(TabKind::Right), StoryMode::Point);
    let tab = span(&l, 0, 2);
    let room = Mp::new(STOP) - width_of("ab");
    assert!(width_of(word) > room, "the word is wider than the room");
    assert_eq!(tab.1 - tab.0, Mp::ZERO, "the tab has no width left");
    assert_eq!(span(&l, 0, 3).0, width_of("ab"), "the text follows at once");
    assert_eq!(l.lines[0].width, width_of("ab") + width_of(word));
}

#[test]
fn a_centre_section_that_overflows_keeps_its_left_half_in_the_room() {
    let stop = Mp::new(30_000);
    let word = "WIDEWORDWIDEWORDWIDEWORD";
    let text = format!("ab\t{word}");
    let l = layout(
        &text,
        ruled(&[TabStop::new(stop, TabKind::Centre)]),
        StoryMode::Point,
    );
    let w = width_of(word);
    let room = stop - width_of("ab");
    assert!(
        w.raw() / 2 > room.raw(),
        "half of the word exceeds the room"
    );
    // All the room is used up and the rest goes right of the stop.
    assert_eq!(span(&l, 0, 3).0, width_of("ab"));
    assert_eq!(l.lines[0].width, stop + (w - room));
}

#[test]
fn each_tab_section_has_its_own_stop() {
    let text = "a\tright\tcentre\t1.5";
    let stops = [
        TabStop::new(Mp::new(60_000), TabKind::Right),
        TabStop::new(Mp::new(140_000), TabKind::Centre),
        TabStop::new(Mp::new(200_000), TabKind::Decimal),
    ];
    let l = layout(text, ruled(&stops), StoryMode::Point);
    assert_eq!(span(&l, 0, 6).1, Mp::new(60_000), "right: ends at its stop");
    let w = width_of("centre");
    let left = span(&l, 0, 8).0;
    let expect = Mp::new(140_000 - w.raw() / 2);
    assert!(
        (left.raw() - expect.raw()).abs() <= 6,
        "{left:?} vs {expect:?}"
    );
    assert_eq!(
        span(&l, 0, 16).0,
        Mp::new(200_000),
        "decimal point at 200 pt"
    );
}

#[test]
fn a_right_section_that_fits_left_of_the_stop_does_not_wrap() {
    // About 45 pt of text. After a left stop at 100 pt it would end past the
    // 120 pt column and wrap; right aligned it lies left of the stop.
    let text = "\tAAAA BBBB";
    let col = StoryMode::column(Mp::new(120_000));
    let right = layout(text, one(TabKind::Right), col);
    assert_eq!(right.lines.len(), 1);
    let left = layout(text, one(TabKind::Left), col);
    assert!(left.lines.len() > 1);
}

#[test]
fn a_wrapped_line_finishes_its_section_at_the_break() {
    // The first line holds only what fitted; its right section ends at the
    // stop although more text follows on the next line.
    let text = "\tAAAA BBBB CCCC DDDD EEEE FFFF GGGG HHHH";
    let l = layout(
        text,
        one(TabKind::Right),
        StoryMode::column(Mp::new(100_000)),
    );
    assert!(l.lines.len() > 1);
    let first = &l.lines[0];
    let trimmed = text[first.logical_range.clone()].trim_end();
    let last = first
        .clusters
        .iter()
        .filter(|c| c.range.end <= first.logical_range.start + trimmed.len())
        .map(|c| c.x + c.width)
        .max()
        .unwrap();
    assert!(
        last <= Mp::new(STOP) + Mp::new(1),
        "ends at the stop: {last:?}"
    );
}

#[test]
fn past_the_ruler_the_stops_are_left_stops_on_the_grid() {
    let l = layout("a\t\tb", one(TabKind::Right), StoryMode::Point);
    // The first tab reaches the right stop (no text between the tabs), the
    // second finds the next half inch strictly beyond it.
    assert_eq!(span(&l, 0, 3).0, Mp::new(108_000));
}
