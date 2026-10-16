//! The text tool's infobar, OpenType feature panel and ruler (phase 9,
//! T9.4.9–T9.4.10), as data: what the bar shows for the text being edited
//! and what an edit of it sets. The tool ([`crate::text_tool`]) decides
//! where an edit goes (the selection, the caret's pending style, the
//! selected stories or the current attributes); this module only reads
//! attribute values and turns infobar values into attribute values.
//!
//! # What the bar shows
//!
//! Per text slot, the value in force over the target, or nothing when the
//! target mixes several (a selection across two sizes shows an empty size
//! field). At a caret it is the character before the caret — what typing
//! there continues — overlaid with the caret's pending style. Paragraph
//! attributes come from the first line of each paragraph the target
//! touches, as layout reads them.

use std::ops::Range;
use std::sync::Arc;

use xarast_doc::{
    AttrSlot, AttrValue, FeatureSetting, Justification, LineSpacing, ResolvedAttrs, StoryText,
    TabStop, TypefaceRef,
};
use xarast_geom::Mp;

use crate::tool::{FeatureOption, InfobarField, InfobarItem, InfobarValue, TextRuler};

/// The features of the OpenType panel: tag, label, and whether a font
/// applies it when nothing says otherwise (only the standard ligatures,
/// of these).
pub const FEATURES: [([u8; 4], &str, bool); 12] = [
    (*b"liga", "Standard ligatures", true),
    (*b"dlig", "Discretionary ligatures", false),
    (*b"smcp", "Small capitals", false),
    (*b"c2sc", "Capitals to small capitals", false),
    (*b"onum", "Oldstyle figures", false),
    (*b"lnum", "Lining figures", false),
    (*b"tnum", "Tabular figures", false),
    (*b"pnum", "Proportional figures", false),
    (*b"frac", "Fractions", false),
    (*b"zero", "Slashed zero", false),
    (*b"swsh", "Swashes", false),
    (*b"ss01", "Stylistic set 1", false),
];

/// The alignment choices, in [`Justification`] order.
pub const JUSTIFY: [&str; 4] = ["Left", "Centre", "Right", "Full"];

/// The tab stop kinds, indexed by the format's `kind & 3`.
pub const TAB_KINDS: [&str; 4] = ["Left", "Right", "Centre", "Decimal"];

/// The text slots the bar reads.
const TEXT_SLOTS: [AttrSlot; 16] = [
    AttrSlot::TxtFontTypeface,
    AttrSlot::TxtBold,
    AttrSlot::TxtItalic,
    AttrSlot::TxtAspectRatio,
    AttrSlot::TxtJustification,
    AttrSlot::TxtTracking,
    AttrSlot::TxtUnderline,
    AttrSlot::TxtFontSize,
    AttrSlot::TxtScript,
    AttrSlot::TxtBaseline,
    AttrSlot::TxtLineSpace,
    AttrSlot::TxtLeftMargin,
    AttrSlot::TxtRightMargin,
    AttrSlot::TxtFirstIndent,
    AttrSlot::TxtRuler,
    AttrSlot::TxtFeatures,
];

/// The values in force over a target, per text slot: every distinct value
/// met, in order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Values {
    slots: Vec<(AttrSlot, Vec<AttrValue>)>,
}

impl Values {
    /// Records `v` as met in its slot.
    pub fn add(&mut self, v: &AttrValue) {
        let Some(slot) = v.slot() else {
            return;
        };
        match self.slots.iter_mut().find(|(s, _)| *s == slot) {
            Some((_, vs)) => {
                if !vs.contains(v) {
                    vs.push(v.clone());
                }
            }
            None => self.slots.push((slot, vec![v.clone()])),
        }
    }

    /// Records every text slot of a resolved attribute set.
    pub fn add_resolved(&mut self, a: &ResolvedAttrs, slots: &[AttrSlot]) {
        for s in slots {
            self.add(a.get(*s));
        }
    }

    /// Replaces a slot's values by one: a pending style over what the text
    /// has.
    pub fn force(&mut self, v: &AttrValue) {
        if let Some(slot) = v.slot() {
            self.slots.retain(|(s, _)| *s != slot);
            self.slots.push((slot, vec![v.clone()]));
        }
    }

    /// The one value of a slot, or `None` when the target mixes several
    /// (or never met it).
    #[must_use]
    pub fn one(&self, slot: AttrSlot) -> Option<&AttrValue> {
        self.slots
            .iter()
            .find(|(s, _)| *s == slot)
            .and_then(|(_, vs)| match vs.as_slice() {
                [v] => Some(v),
                _ => None,
            })
    }

    /// Every value met of a slot.
    #[must_use]
    pub fn all(&self, slot: AttrSlot) -> &[AttrValue] {
        self.slots
            .iter()
            .find(|(s, _)| *s == slot)
            .map_or(&[], |(_, vs)| vs.as_slice())
    }
}

/// The character slots (everything but the paragraph slots).
fn char_slots() -> impl Iterator<Item = AttrSlot> {
    TEXT_SLOTS
        .into_iter()
        .filter(|s| !xarast_doc::is_paragraph_slot(*s))
}

fn para_slots() -> impl Iterator<Item = AttrSlot> {
    TEXT_SLOTS
        .into_iter()
        .filter(|s| xarast_doc::is_paragraph_slot(*s))
}

/// The run whose style typing at a caret on `at` continues: the character
/// before it, unless that is a paragraph break (then the one after).
fn caret_run(st: &StoryText, at: usize) -> Option<usize> {
    let before = st.text[..at.min(st.text.len())].chars().next_back();
    match before {
        Some(c) if c != '\n' => st.run_at(at - c.len_utf8()),
        _ => st.run_at(at),
    }
}

/// The values over a byte range of a story (a caret when it is empty).
#[must_use]
pub fn story_values(st: &StoryText, range: &Range<usize>) -> Values {
    let mut v = Values::default();
    let chars: Vec<AttrSlot> = char_slots().collect();
    if range.is_empty() {
        match caret_run(st, range.start) {
            Some(i) => v.add_resolved(&st.runs[i].attrs, &chars),
            None => v.add_resolved(&st.story_attrs, &chars),
        }
    } else {
        let mut any = false;
        for r in &st.runs {
            if r.range.start < range.end && r.range.end > range.start {
                v.add_resolved(&r.attrs, &chars);
                any = true;
            }
        }
        if !any {
            v.add_resolved(&st.story_attrs, &chars);
        }
    }
    let paras: Vec<AttrSlot> = para_slots().collect();
    let lines = xarast_doc::paragraph_line_range(st, range);
    if lines.is_empty() {
        v.add_resolved(&st.story_attrs, &paras);
    }
    for i in lines.clone() {
        let first = i == lines.start || st.lines[i - 1].ends_paragraph;
        if first {
            v.add_resolved(&st.lines[i].attrs, &paras);
        }
    }
    v
}

/// The values of a whole set of attributes: the current attributes over
/// the document's defaults.
#[must_use]
pub fn plain_values(defaults: &xarast_doc::DefaultAttrs, current: &[AttrValue]) -> Values {
    let mut v = Values::default();
    for s in TEXT_SLOTS {
        v.add(defaults.get(s));
    }
    for c in current {
        if c.slot().is_some_and(xarast_doc::is_text_slot) {
            v.force(c);
        }
    }
    v
}

/// Whether a feature is on in a settings list: its value when set, the
/// font's default otherwise.
#[must_use]
pub fn feature_on(list: &[FeatureSetting], tag: [u8; 4]) -> bool {
    match list.iter().find(|s| s.tag == tag) {
        Some(s) => s.value > 0,
        None => FEATURES
            .iter()
            .find(|(t, _, _)| *t == tag)
            .is_some_and(|(_, _, d)| *d),
    }
}

/// A settings list with one feature switched: the setting is dropped when
/// it matches the font's default, so the list stays as short as it can.
#[must_use]
pub fn with_feature(list: &[FeatureSetting], tag: [u8; 4], on: bool) -> Arc<[FeatureSetting]> {
    let default = FEATURES
        .iter()
        .find(|(t, _, _)| *t == tag)
        .is_some_and(|(_, _, d)| *d);
    let mut v: Vec<FeatureSetting> = list.iter().copied().filter(|s| s.tag != tag).collect();
    if on != default {
        v.push(FeatureSetting {
            tag,
            value: u16::from(on),
        });
    }
    FeatureSetting::normalised(&v)
}

fn features_of(v: &AttrValue) -> &[FeatureSetting] {
    match v {
        AttrValue::FontFeatures(f) => f,
        _ => &[],
    }
}

/// The per-run edits that switch one feature over a byte range, keeping
/// each run's other settings.
#[must_use]
pub fn feature_edits(
    st: &StoryText,
    range: &Range<usize>,
    tag: [u8; 4],
    on: bool,
) -> Vec<(Range<usize>, AttrValue)> {
    st.runs
        .iter()
        .filter(|r| r.range.start < range.end && r.range.end > range.start)
        .filter_map(|r| {
            let list = features_of(r.attrs.get(AttrSlot::TxtFeatures));
            (feature_on(list, tag) != on).then(|| {
                (
                    r.range.start.max(range.start)..r.range.end.min(range.end),
                    AttrValue::FontFeatures(with_feature(list, tag, on)),
                )
            })
        })
        .collect()
}

/// The bar's items for `values`. `families` is the installed list the font
/// chooser offers; `ruler` the ruler of the caret's paragraph.
#[must_use]
pub fn items(
    values: &Values,
    families: Arc<[Arc<str>]>,
    ruler: Option<TextRuler>,
) -> Vec<InfobarItem> {
    let family = values.one(AttrSlot::TxtFontTypeface).and_then(|v| match v {
        AttrValue::FontTypeface(t) => Some(Arc::clone(&t.family)),
        _ => None,
    });
    let size = values.one(AttrSlot::TxtFontSize).and_then(|v| match v {
        AttrValue::FontSize(s) => Some(s.to_f64() / 1000.0),
        _ => None,
    });
    let toggle = |field, slot| {
        let on = matches!(
            values.one(slot),
            Some(AttrValue::Bold(true) | AttrValue::Italic(true) | AttrValue::Underline(true))
        );
        InfobarItem::Toggle { field, on }
    };
    let justify = values
        .one(AttrSlot::TxtJustification)
        .and_then(|v| match v {
            AttrValue::Justification(j) => Some(*j as usize),
            _ => None,
        });
    let (spacing, spacing_suffix) = match values.one(AttrSlot::TxtLineSpace) {
        Some(AttrValue::LineSpace(LineSpacing::Ratio(r))) => (Some(f64::from(*r) * 100.0), "%"),
        Some(AttrValue::LineSpace(LineSpacing::Absolute(m))) if m.raw() > 0 => {
            (Some(m.to_f64() / 1000.0), "pt")
        }
        Some(AttrValue::LineSpace(LineSpacing::Absolute(_))) => (Some(100.0), "%"),
        _ => (None, "%"),
    };
    let tracking = values.one(AttrSlot::TxtTracking).and_then(|v| match v {
        AttrValue::Tracking(t) => Some(f64::from(t.raw())),
        _ => None,
    });
    let lists = values.all(AttrSlot::TxtFeatures);
    let options = FEATURES
        .iter()
        .map(|(tag, label, _)| {
            let mut ons = lists.iter().map(|v| feature_on(features_of(v), *tag));
            let first = ons.next();
            let on = match first {
                Some(f) if ons.all(|o| o == f) => Some(f),
                Some(_) => None,
                None => Some(feature_on(&[], *tag)),
            };
            FeatureOption {
                tag: *tag,
                label,
                on,
            }
        })
        .collect();
    let mut out = vec![
        InfobarItem::FontFamily {
            field: InfobarField::TextFont,
            families,
            selected: family,
        },
        InfobarItem::Scalar {
            field: InfobarField::TextSize,
            value: size,
            suffix: "pt",
            min: MIN_SIZE_PT,
            max: MAX_SIZE_PT,
        },
        toggle(InfobarField::TextBold, AttrSlot::TxtBold),
        toggle(InfobarField::TextItalic, AttrSlot::TxtItalic),
        toggle(InfobarField::TextUnderline, AttrSlot::TxtUnderline),
        InfobarItem::Choice {
            field: InfobarField::TextJustify,
            options: JUSTIFY.to_vec(),
            selected: justify,
        },
        InfobarItem::Scalar {
            field: InfobarField::TextLineSpacing,
            value: spacing,
            suffix: spacing_suffix,
            min: 0.0,
            max: 10_000.0,
        },
        InfobarItem::Scalar {
            field: InfobarField::TextTracking,
            value: tracking,
            suffix: "",
            min: -1_000.0,
            max: 10_000.0,
        },
        InfobarItem::Features { options },
    ];
    if let Some(r) = ruler {
        out.push(InfobarItem::Choice {
            field: InfobarField::TextTabKind,
            options: TAB_KINDS.to_vec(),
            selected: Some(usize::from(r.tab_kind & 3)),
        });
        out.push(InfobarItem::TextRuler(r));
    }
    out
}

/// The smallest text size the bar accepts, in points.
pub const MIN_SIZE_PT: f64 = 0.5;
/// The largest, in points.
pub const MAX_SIZE_PT: f64 = 5_000.0;

/// The attribute value a character or paragraph field of the bar sets.
/// `None` for a field or value that sets no attribute here (features,
/// the ruler's tab stops and the tab kind have their own paths).
#[must_use]
pub fn value_for(
    field: InfobarField,
    value: InfobarValue,
    shown: &Values,
    families: &[Arc<str>],
) -> Option<AttrValue> {
    Some(match (field, value) {
        (InfobarField::TextFont, InfobarValue::Choice(i)) => {
            let name = families.get(i)?;
            AttrValue::FontTypeface(Arc::new(TypefaceRef {
                full_name: Arc::clone(name),
                family: Arc::clone(name),
                panose: None,
            }))
        }
        (InfobarField::TextSize, InfobarValue::Real(pt)) if pt.is_finite() => AttrValue::FontSize(
            Mp::from_f64_round(pt.clamp(MIN_SIZE_PT, MAX_SIZE_PT) * 1000.0),
        ),
        (InfobarField::TextBold, InfobarValue::Toggle(b)) => AttrValue::Bold(b),
        (InfobarField::TextItalic, InfobarValue::Toggle(b)) => AttrValue::Italic(b),
        (InfobarField::TextUnderline, InfobarValue::Toggle(b)) => AttrValue::Underline(b),
        (InfobarField::TextJustify, InfobarValue::Choice(i)) => AttrValue::Justification(match i {
            0 => Justification::Left,
            1 => Justification::Centre,
            2 => Justification::Right,
            3 => Justification::Full,
            _ => return None,
        }),
        (InfobarField::TextLineSpacing, InfobarValue::Real(v)) if v.is_finite() && v >= 0.0 => {
            // Points when the paragraph spaces absolutely, per cent else.
            match shown.one(AttrSlot::TxtLineSpace) {
                Some(AttrValue::LineSpace(LineSpacing::Absolute(m))) if m.raw() > 0 => {
                    AttrValue::LineSpace(LineSpacing::Absolute(Mp::from_f64_round(v * 1000.0)))
                }
                _ => AttrValue::LineSpace(LineSpacing::Ratio((v / 100.0) as f32)),
            }
        }
        (InfobarField::TextTracking, InfobarValue::Real(v)) if v.is_finite() => {
            AttrValue::Tracking(Mp::new(v.round().clamp(-1_000.0, 10_000.0) as i32))
        }
        (InfobarField::TextLeftMargin, InfobarValue::Length(m)) => {
            AttrValue::LeftMargin(m.max(Mp::ZERO))
        }
        (InfobarField::TextRightMargin, InfobarValue::Length(m)) => {
            AttrValue::RightMargin(m.max(Mp::ZERO))
        }
        (InfobarField::TextFirstIndent, InfobarValue::Length(m)) => {
            AttrValue::FirstIndent(m.max(Mp::ZERO))
        }
        _ => return None,
    })
}

/// The tab stops a ruler edit leaves, from the stops in force. `None`
/// when the field is not a tab edit or names no stop.
#[must_use]
pub fn tabs_after(
    field: InfobarField,
    value: InfobarValue,
    tabs: &[TabStop],
    kind: u8,
) -> Option<Arc<[TabStop]>> {
    let mut v: Vec<TabStop> = tabs.to_vec();
    match (field, value) {
        (InfobarField::TextTabAdd, InfobarValue::Length(at)) => v.push(TabStop {
            position: at.max(Mp::ZERO),
            kind,
            decimal: (kind & 3 == 3).then_some('.'),
        }),
        (InfobarField::TextTabMove(i), InfobarValue::Length(at)) => {
            v.get_mut(usize::from(i))?.position = at.max(Mp::ZERO);
        }
        (InfobarField::TextTabRemove(i), _) => {
            if usize::from(i) >= v.len() {
                return None;
            }
            v.remove(usize::from(i));
        }
        _ => return None,
    }
    v.sort_by_key(|t| t.position);
    // Two stops at one place are one.
    v.dedup_by_key(|t| t.position);
    Some(Arc::from(v))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_feature_set_to_its_default_leaves_the_list() {
        let smcp = with_feature(&[], *b"smcp", true);
        assert_eq!(smcp.len(), 1);
        assert!(feature_on(&smcp, *b"smcp"));
        assert!(feature_on(&smcp, *b"liga"), "ligatures default on");
        let no_liga = with_feature(&smcp, *b"liga", false);
        assert_eq!(no_liga.len(), 2);
        assert!(!feature_on(&no_liga, *b"liga"));
        let back = with_feature(&with_feature(&no_liga, *b"liga", true), *b"smcp", false);
        assert!(back.is_empty());
    }

    #[test]
    fn tab_edits_keep_the_stops_sorted_and_single() {
        let t = |p: i32, kind: u8| TabStop {
            position: Mp::new(p),
            kind,
            decimal: None,
        };
        let tabs = [t(36_000, 0), t(72_000, 1)];
        let add = tabs_after(
            InfobarField::TextTabAdd,
            InfobarValue::Length(Mp::new(50_000)),
            &tabs,
            2,
        )
        .unwrap();
        assert_eq!(&*add, [t(36_000, 0), t(50_000, 2), t(72_000, 1)]);
        let moved = tabs_after(
            InfobarField::TextTabMove(0),
            InfobarValue::Length(Mp::new(90_000)),
            &tabs,
            0,
        )
        .unwrap();
        assert_eq!(&*moved, [t(72_000, 1), t(90_000, 0)]);
        let gone = tabs_after(
            InfobarField::TextTabRemove(1),
            InfobarValue::Toggle(false),
            &tabs,
            0,
        )
        .unwrap();
        assert_eq!(&*gone, [t(36_000, 0)]);
        assert!(
            tabs_after(
                InfobarField::TextTabRemove(5),
                InfobarValue::Toggle(false),
                &tabs,
                0
            )
            .is_none()
        );
    }

    #[test]
    fn line_spacing_follows_the_paragraph_s_kind() {
        let mut shown = Values::default();
        shown.add(&AttrValue::LineSpace(LineSpacing::Ratio(1.0)));
        assert_eq!(
            value_for(
                InfobarField::TextLineSpacing,
                InfobarValue::Real(150.0),
                &shown,
                &[]
            ),
            Some(AttrValue::LineSpace(LineSpacing::Ratio(1.5)))
        );
        let mut shown = Values::default();
        shown.add(&AttrValue::LineSpace(LineSpacing::Absolute(Mp::new(
            20_000,
        ))));
        assert_eq!(
            value_for(
                InfobarField::TextLineSpacing,
                InfobarValue::Real(18.0),
                &shown,
                &[]
            ),
            Some(AttrValue::LineSpace(LineSpacing::Absolute(Mp::new(18_000))))
        );
    }
}
