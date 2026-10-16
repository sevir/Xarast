//! The text tool's infobar, OpenType panel and ruler (phase 9, T9.4.9 and
//! T9.4.10), driven through intents as the shell drives them: what the bar
//! shows, where its edits go (selection, caret, pending caret, selected
//! stories, current attributes) and that each is one undoable step.

use std::path::Path;
use std::sync::Arc;

use xarast_app::fonts::FontService;
use xarast_app::text_tool::{TextEditing, TextSelection};
use xarast_app::{
    DevicePoint, DocumentId, InfobarField, InfobarItem, InfobarValue, Intent, PointerButton,
    PointerSample, Session, TextInput, TextInputKind, TextKey, TextNav, TextRuler, ToolId,
};
use xarast_doc::builder::{BuildLimits, skeleton};
use xarast_doc::{
    AttrSlot, AttrValue, Justification, NodeId, NodeKind, StoryText, TabStop, TextItem, TextLayout,
    TextStoryNode, TypefaceRef,
};
use xarast_geom::{Matrix, Mp, Point, Vector};

fn fonts() -> Arc<FontService> {
    FontService::from_dir(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../xarast-text/tests/fonts"))
}

const X0: i32 = 100_000;
const Y0: i32 = 400_000;
const WIDTH: i32 = 300_000;

/// One 20 pt Noto Sans column at (100 pt, 400 pt), 300 pt wide: "Hello
/// world" and "Second line" as two paragraphs.
fn fixture() -> (Session, NodeId) {
    xarast_app::fonts::set_shared(fonts());
    let mut b = skeleton(BuildLimits::default()).unwrap();
    let story = b
        .node(NodeKind::TextStory(Box::new(TextStoryNode {
            transform: Matrix::translate(Vector::new(Mp::new(X0), Mp::new(Y0))),
            layout: TextLayout::InColumn {
                width: Mp::new(WIDTH),
                word_wrap: true,
            },
            ..TextStoryNode::default()
        })))
        .unwrap()
        .node_id();
    b.push_scope().unwrap();
    b.attribute(AttrValue::FontTypeface(Arc::new(TypefaceRef {
        full_name: Arc::from("Noto Sans"),
        family: Arc::from("Noto Sans"),
        panose: None,
    })))
    .unwrap();
    b.attribute(AttrValue::FontSize(Mp::new(20_000))).unwrap();
    for line in ["Hello world", "Second line"] {
        b.node(NodeKind::TextLine(Box::default())).unwrap();
        b.push_scope().unwrap();
        for c in line.chars() {
            b.node(NodeKind::TextItem(TextItem::Char(c))).unwrap();
        }
        b.node(NodeKind::TextItem(TextItem::LineBreak(true)))
            .unwrap();
        b.pop_scope();
    }
    b.pop_scope();
    let (doc, _) = b.finish().unwrap();
    let mut s = Session::adopt(DocumentId(1), doc, None);
    s.apply(Intent::SetZoom {
        zoom: 1.0,
        anchor: None,
    })
    .unwrap();
    let c = s.viewport.doc_to_device(Point::raw(X0 + 60_000, Y0));
    let size = s.viewport.size();
    s.apply(Intent::Pan {
        dx: f64::from(size.width) / 2.0 - c.x,
        dy: f64::from(size.height) / 2.0 - c.y,
    })
    .unwrap();
    s.apply(Intent::ChooseTool(ToolId::Text)).unwrap();
    (s, story)
}

fn sample(at: DevicePoint, t: u64) -> PointerSample {
    PointerSample {
        at,
        pressure: None,
        time_ms: t,
    }
}

fn click(s: &mut Session, x: i32, y: i32, t: u64) {
    let at = s.viewport.doc_to_device(Point::raw(x, y));
    s.apply(Intent::PointerMove(sample(at, t))).unwrap();
    s.apply(Intent::PointerDown {
        button: PointerButton::Primary,
        sample: sample(at, t),
    })
    .unwrap();
    s.apply(Intent::PointerUp {
        button: PointerButton::Primary,
        sample: sample(at, t),
    })
    .unwrap();
}

fn nav(s: &mut Session, key: TextKey, word: bool, extend: bool) {
    s.apply(Intent::TextNav(TextNav { key, word, extend }))
        .unwrap();
}

fn selection(s: &Session) -> TextSelection {
    match s.text_state() {
        Some(TextEditing::Story(sel)) => sel,
        other => panic!("not editing a story: {other:?}"),
    }
}

/// A caret at the story's start, then `extend` characters selected.
fn select_from_start(s: &mut Session, extend: usize) {
    click(s, X0 + 60_000, Y0 + 5_000, 0);
    nav(s, TextKey::Home, true, false);
    for _ in 0..extend {
        nav(s, TextKey::Right, false, true);
    }
    assert_eq!(selection(s).range(), 0..extend);
}

fn edit(s: &mut Session, field: InfobarField, value: InfobarValue) {
    s.apply(Intent::InfobarEdit { field, value }).unwrap();
}

fn type_str(s: &mut Session, text: &str, time_ms: u64) {
    s.apply(Intent::TextInput(TextInput {
        kind: TextInputKind::Insert(text.to_owned()),
        time_ms,
    }))
    .unwrap();
}

fn steps(s: &Session) -> usize {
    s.bus.history().len()
}

fn st(s: &Session, story: NodeId) -> StoryText {
    StoryText::collect_simple(&s.doc.tree, &s.doc.defaults, story).unwrap()
}

/// The value of `slot` for every character of the story's text.
fn per_char(s: &Session, story: NodeId, slot: AttrSlot) -> Vec<AttrValue> {
    let t = st(s, story);
    t.text
        .char_indices()
        .map(|(i, _)| t.runs[t.run_at(i).unwrap()].attrs.get(slot).clone())
        .collect()
}

fn item(s: &Session, field: InfobarField) -> InfobarItem {
    s.infobar()
        .items
        .into_iter()
        .find(|i| match i {
            InfobarItem::Toggle { field: f, .. }
            | InfobarItem::Choice { field: f, .. }
            | InfobarItem::Scalar { field: f, .. }
            | InfobarItem::FontFamily { field: f, .. } => *f == field,
            _ => false,
        })
        .unwrap_or_else(|| panic!("no {field:?} in the bar"))
}

fn scalar(s: &Session, field: InfobarField) -> Option<f64> {
    match item(s, field) {
        InfobarItem::Scalar { value, .. } => value,
        other => panic!("{other:?}"),
    }
}

fn toggle(s: &Session, field: InfobarField) -> bool {
    match item(s, field) {
        InfobarItem::Toggle { on, .. } => on,
        other => panic!("{other:?}"),
    }
}

fn ruler(s: &Session) -> Option<TextRuler> {
    s.infobar().items.into_iter().find_map(|i| match i {
        InfobarItem::TextRuler(r) => Some(r),
        _ => None,
    })
}

fn feature(s: &Session, tag: &[u8; 4]) -> Option<bool> {
    s.infobar()
        .items
        .into_iter()
        .find_map(|i| match i {
            InfobarItem::Features { options } => options.into_iter().find(|o| o.tag == *tag),
            _ => None,
        })
        .unwrap()
        .on
}

#[test]
fn the_bar_shows_the_text_at_the_caret_and_a_mixed_selection_empty() {
    let (mut s, _) = fixture();
    select_from_start(&mut s, 0);
    match item(&s, InfobarField::TextFont) {
        InfobarItem::FontFamily {
            families, selected, ..
        } => {
            assert_eq!(selected.as_deref(), Some("Noto Sans"));
            assert!(families.iter().any(|f| &**f == "Noto Sans"));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(scalar(&s, InfobarField::TextSize), Some(20.0));
    assert!(!toggle(&s, InfobarField::TextBold));
    assert_eq!(scalar(&s, InfobarField::TextLineSpacing), Some(100.0));
    assert_eq!(scalar(&s, InfobarField::TextTracking), Some(0.0));
    assert_eq!(feature(&s, b"liga"), Some(true));
    assert_eq!(feature(&s, b"smcp"), Some(false));
    let r = ruler(&s).expect("a straight column shows its ruler");
    assert_eq!((r.origin, r.width), (Mp::new(X0), Some(Mp::new(WIDTH))));
    assert!(r.tabs.is_empty());
    // A selection across two sizes shows no size.
    select_from_start(&mut s, 3);
    edit(&mut s, InfobarField::TextSize, InfobarValue::Real(30.0));
    select_from_start(&mut s, 5);
    assert_eq!(scalar(&s, InfobarField::TextSize), None);
}

#[test]
fn a_character_attribute_on_a_selection_is_one_undoable_step() {
    let (mut s, story) = fixture();
    let before = s.doc.canonical_digest();
    select_from_start(&mut s, 5);
    edit(&mut s, InfobarField::TextBold, InfobarValue::Toggle(true));
    assert_eq!(steps(&s), 1);
    assert_eq!(s.undo_label(), Some("Bold"));
    let bold = per_char(&s, story, AttrSlot::TxtBold);
    assert!(bold[..5].iter().all(|v| *v == AttrValue::Bold(true)));
    assert!(bold[5..].iter().all(|v| *v == AttrValue::Bold(false)));
    assert!(toggle(&s, InfobarField::TextBold), "the bar follows");
    // The same value again records nothing.
    edit(&mut s, InfobarField::TextBold, InfobarValue::Toggle(true));
    assert_eq!(steps(&s), 1);
    edit(&mut s, InfobarField::TextSize, InfobarValue::Real(12.5));
    edit(&mut s, InfobarField::TextTracking, InfobarValue::Real(80.0));
    assert_eq!(steps(&s), 3);
    assert_eq!(
        per_char(&s, story, AttrSlot::TxtFontSize)[4],
        AttrValue::FontSize(Mp::new(12_500))
    );
    assert_eq!(
        per_char(&s, story, AttrSlot::TxtTracking)[0],
        AttrValue::Tracking(Mp::new(80))
    );
    // The selection survives the edits.
    assert_eq!(selection(&s).range(), 0..5);
    for _ in 0..3 {
        s.apply(Intent::Undo).unwrap();
    }
    assert_eq!(s.doc.canonical_digest(), before);
}

#[test]
fn attributes_chosen_at_a_caret_style_the_text_typed_there() {
    let (mut s, story) = fixture();
    let before = s.doc.canonical_digest();
    select_from_start(&mut s, 0);
    nav(&mut s, TextKey::Right, true, false); // after "Hello"
    let at = selection(&s).head.byte;
    edit(&mut s, InfobarField::TextBold, InfobarValue::Toggle(true));
    edit(&mut s, InfobarField::TextSize, InfobarValue::Real(30.0));
    // Nothing in the document yet; the bar shows the caret's style.
    assert_eq!(s.doc.canonical_digest(), before);
    assert!(s.undo_label().is_none());
    assert!(toggle(&s, InfobarField::TextBold));
    assert_eq!(scalar(&s, InfobarField::TextSize), Some(30.0));
    type_str(&mut s, "X", 1_000);
    type_str(&mut s, "Y", 1_100);
    let bold = per_char(&s, story, AttrSlot::TxtBold);
    let size = per_char(&s, story, AttrSlot::TxtFontSize);
    for i in [at, at + 1] {
        assert_eq!(bold[i], AttrValue::Bold(true));
        assert_eq!(size[i], AttrValue::FontSize(Mp::new(30_000)));
    }
    assert_eq!(bold[at - 1], AttrValue::Bold(false));
    assert_eq!(bold[at + 2], AttrValue::Bold(false));
    // Styling and typing are one step.
    assert_eq!(steps(&s), 1);
    assert_eq!(s.undo_label(), Some("Typing"));
    s.apply(Intent::Undo).unwrap();
    assert_eq!(s.doc.canonical_digest(), before);
    // A caret move drops a pending style.
    nav(&mut s, TextKey::Home, true, false);
    edit(&mut s, InfobarField::TextItalic, InfobarValue::Toggle(true));
    nav(&mut s, TextKey::Right, false, false);
    assert!(!toggle(&s, InfobarField::TextItalic));
}

#[test]
fn a_pending_caret_s_style_goes_to_the_story_typing_creates() {
    let (mut s, _) = fixture();
    let before = s.doc.canonical_digest();
    click(&mut s, 300_000, 200_000, 0);
    edit(&mut s, InfobarField::TextSize, InfobarValue::Real(30.0));
    edit(&mut s, InfobarField::TextItalic, InfobarValue::Toggle(true));
    edit(&mut s, InfobarField::TextJustify, InfobarValue::Choice(2));
    assert_eq!(s.doc.canonical_digest(), before);
    assert_eq!(scalar(&s, InfobarField::TextSize), Some(30.0));
    type_str(&mut s, "Hi", 1_000);
    let new = selection(&s).story;
    assert_eq!(
        per_char(&s, new, AttrSlot::TxtFontSize),
        vec![AttrValue::FontSize(Mp::new(30_000)); 3]
    );
    assert_eq!(
        per_char(&s, new, AttrSlot::TxtItalic)[0],
        AttrValue::Italic(true)
    );
    assert_eq!(
        st(&s, new).lines[0].attrs.get(AttrSlot::TxtJustification),
        &AttrValue::Justification(Justification::Right)
    );
    assert_eq!(steps(&s), 1);
    // The current attributes are not changed by it.
    assert!(s.edit.current.values().is_empty());
}

#[test]
fn paragraph_attributes_and_the_ruler_edit_the_caret_s_paragraph() {
    let (mut s, story) = fixture();
    let before = s.doc.canonical_digest();
    select_from_start(&mut s, 0);
    nav(&mut s, TextKey::Down, false, false);
    let just = |s: &Session, l: usize| {
        st(s, story).lines[l]
            .attrs
            .get(AttrSlot::TxtJustification)
            .clone()
    };
    edit(&mut s, InfobarField::TextJustify, InfobarValue::Choice(1));
    assert_eq!(just(&s, 0), AttrValue::Justification(Justification::Left));
    assert_eq!(just(&s, 1), AttrValue::Justification(Justification::Centre));
    assert_eq!(s.undo_label(), Some("Justification"));
    edit(
        &mut s,
        InfobarField::TextLineSpacing,
        InfobarValue::Real(150.0),
    );
    assert_eq!(scalar(&s, InfobarField::TextLineSpacing), Some(150.0));
    // The ruler: margins, indent, tab stops.
    edit(
        &mut s,
        InfobarField::TextLeftMargin,
        InfobarValue::Length(Mp::new(10_000)),
    );
    edit(
        &mut s,
        InfobarField::TextRightMargin,
        InfobarValue::Length(Mp::new(20_000)),
    );
    edit(
        &mut s,
        InfobarField::TextFirstIndent,
        InfobarValue::Length(Mp::new(30_000)),
    );
    edit(&mut s, InfobarField::TextTabKind, InfobarValue::Choice(2));
    edit(
        &mut s,
        InfobarField::TextTabAdd,
        InfobarValue::Length(Mp::new(72_000)),
    );
    edit(
        &mut s,
        InfobarField::TextTabAdd,
        InfobarValue::Length(Mp::new(36_000)),
    );
    let r = ruler(&s).unwrap();
    assert_eq!(
        (r.left_margin, r.right_margin, r.first_indent),
        (Mp::new(10_000), Mp::new(20_000), Mp::new(30_000))
    );
    let tab = |p: i32| TabStop {
        position: Mp::new(p),
        kind: 2,
        decimal: None,
    };
    assert_eq!(r.tabs, [tab(36_000), tab(72_000)]);
    assert_eq!(r.tab_kind, 2);
    edit(
        &mut s,
        InfobarField::TextTabMove(0),
        InfobarValue::Length(Mp::new(100_000)),
    );
    assert_eq!(ruler(&s).unwrap().tabs, [tab(72_000), tab(100_000)]);
    edit(
        &mut s,
        InfobarField::TextTabRemove(1),
        InfobarValue::Toggle(false),
    );
    assert_eq!(ruler(&s).unwrap().tabs, [tab(72_000)]);
    assert_eq!(s.undo_label(), Some("Tab Stops"));
    // The first paragraph is untouched.
    let t = st(&s, story);
    assert_eq!(
        t.lines[0].attrs.get(AttrSlot::TxtLeftMargin),
        &AttrValue::LeftMargin(Mp::ZERO)
    );
    assert_eq!(
        t.lines[1].attrs.get(AttrSlot::TxtLeftMargin),
        &AttrValue::LeftMargin(Mp::new(10_000))
    );
    // Each is one step (choosing the tab kind is none); undoing them all
    // restores the document.
    assert_eq!(steps(&s), 9);
    for _ in 0..9 {
        s.apply(Intent::Undo).unwrap();
    }
    assert_eq!(s.doc.canonical_digest(), before);
}

#[test]
fn the_feature_panel_switches_one_feature_and_keeps_the_others() {
    let (mut s, story) = fixture();
    let before = s.doc.canonical_digest();
    select_from_start(&mut s, 3);
    edit(
        &mut s,
        InfobarField::TextFeature(*b"liga"),
        InfobarValue::Toggle(false),
    );
    select_from_start(&mut s, 11);
    assert_eq!(feature(&s, b"liga"), None, "mixed");
    edit(
        &mut s,
        InfobarField::TextFeature(*b"smcp"),
        InfobarValue::Toggle(true),
    );
    assert_eq!(s.undo_label(), Some("OpenType Features"));
    assert_eq!(feature(&s, b"smcp"), Some(true));
    let f = per_char(&s, story, AttrSlot::TxtFeatures);
    let tags = |v: &AttrValue| match v {
        AttrValue::FontFeatures(f) => f
            .iter()
            .map(|x| format!("{}={}", x.tag_str(), x.value))
            .collect::<Vec<_>>(),
        _ => panic!(),
    };
    assert_eq!(tags(&f[0]), ["liga=0", "smcp=1"]);
    assert_eq!(tags(&f[5]), ["smcp=1"]);
    assert!(tags(&f[12]).is_empty(), "outside the selection");
    s.apply(Intent::Undo).unwrap();
    s.apply(Intent::Undo).unwrap();
    assert_eq!(s.doc.canonical_digest(), before);
}

#[test]
fn with_no_caret_edits_go_to_the_selected_story_or_the_current_attributes() {
    let (mut s, story) = fixture();
    let before = s.doc.canonical_digest();
    select_from_start(&mut s, 0);
    s.apply(Intent::Cancel).unwrap();
    assert!(s.text_state().is_none());
    assert_eq!(s.edit.selection().collect::<Vec<_>>(), [story]);
    edit(&mut s, InfobarField::TextSize, InfobarValue::Real(12.0));
    assert!(
        per_char(&s, story, AttrSlot::TxtFontSize)
            .iter()
            .all(|v| *v == AttrValue::FontSize(Mp::new(12_000)))
    );
    edit(&mut s, InfobarField::TextJustify, InfobarValue::Choice(3));
    assert_eq!(steps(&s), 2);
    s.apply(Intent::Undo).unwrap();
    s.apply(Intent::Undo).unwrap();
    assert_eq!(s.doc.canonical_digest(), before);
    // Nothing selected: the current attributes.
    s.apply(Intent::SelectNone).unwrap();
    edit(&mut s, InfobarField::TextSize, InfobarValue::Real(30.0));
    assert_eq!(s.doc.canonical_digest(), before);
    assert_eq!(
        s.edit.current.values(),
        [AttrValue::FontSize(Mp::new(30_000))]
    );
    assert_eq!(scalar(&s, InfobarField::TextSize), Some(30.0));
}

#[test]
fn a_family_chosen_from_the_list_is_the_one_shown() {
    let (mut s, story) = fixture();
    select_from_start(&mut s, 5);
    let InfobarItem::FontFamily { families, .. } = item(&s, InfobarField::TextFont) else {
        panic!()
    };
    let (i, other) = families
        .iter()
        .enumerate()
        .find(|(_, f)| &***f != "Noto Sans")
        .expect("the pinned set has more than one family");
    edit(&mut s, InfobarField::TextFont, InfobarValue::Choice(i));
    match &per_char(&s, story, AttrSlot::TxtFontTypeface)[0] {
        AttrValue::FontTypeface(t) => assert_eq!(&*t.family, &**other),
        v => panic!("{v:?}"),
    }
    assert_eq!(s.undo_label(), Some("Font"));
}
