//! The text tool (phase 9, W9.4 T9.4.1, with T9.4.4's keyboard navigation
//! and the mouse gestures of T9.4.5).
//!
//! # The state machine
//!
//! ```text
//!             click on text                 drag on text
//!   Idle ─────────────────────▶ Story ◀──────────────────── (select by drag)
//!    │ ▲                          │ ▲  arrows, Home/End, PgUp/PgDn, Shift extends,
//!    │ │ Esc                      │ │  Ctrl+A, double click word, triple click line
//!    │ └──────────────────────────┘ │
//!    │ click on empty canvas         │ click on empty canvas
//!    ▼                               ▼
//!   Pending { at, column: None } ◀──┘    drag on empty canvas ─▶ Pending { column: Some(width) }
//! ```
//!
//! * **Story**: a caret (and maybe a selection) in an existing story. The
//!   caret is [`TextSelection`], tool state only: nothing is written to the
//!   document (`research/02 §10.11`, "Change 1").
//! * **Pending**: a caret where typing *will* create a new story, a point
//!   story or a column of the dragged width. No story is created until the
//!   first character is typed (T9.4.6), so leaving the tool never leaves an
//!   empty story behind and a stray click costs no undo step. The other way
//!   round, a deletion or cut that takes all of a story's text removes the
//!   story in the same undo step (XARA-T-0237) and the caret becomes a
//!   pending one where the story began, keeping its style; a story on a
//!   path leaves its path as a shape and editing ends.
//!
//! The tool never mutates the document (tools invariant 1): typing and
//! deleting emit [`EditCommand::TypeText`], [`EditCommand::DeleteText`]
//! and, at a pending caret, [`EditCommand::CreateText`], which the session
//! runs through the bus. Delete, Backspace and Enter belong to the text
//! while a caret is up, so they never delete the story object under it.
//!
//! # Typing bursts (T9.4.6)
//!
//! Keys of one kind (typing, or deleting) less than [`TYPING_BURST_MS`]
//! apart, each starting where the previous one left the caret, make one
//! undo step: they share a burst number, which is their coalesce key.
//! Anything else ends the burst — a caret move, a click, a selection,
//! another command changing the document (the tool notes the document's
//! epoch after its own edit), switching between typing and deleting. The
//! first burst at a pending caret creates the story, so one undo removes
//! the story with everything typed into it.
//!
//! # Text attributes (T9.4.9, T9.4.10)
//!
//! The infobar ([`crate::text_infobar`]) shows the attributes of what is
//! being edited and its edits go, in order of preference, to:
//!
//! * the **selection**: one [`EditCommand::SetTextAttr`] step (a character
//!   attribute on the selected characters, a paragraph attribute on the
//!   paragraphs they touch);
//! * a **caret**: a paragraph attribute to its paragraph at once; a
//!   character attribute becomes the caret's *pending style*, which the
//!   next typed text gets (merged into that typing burst's undo step) and
//!   which any caret move drops. A pending caret keeps its style for the
//!   story typing creates;
//! * with no caret, the **selected stories**, whole;
//! * with nothing selected, the **current attributes** new text gets.
//!
//! The text ruler (margins, first-line indent, tab stops of the caret's
//! paragraph) is part of the same description and raises the same edits.
//!
//! # Input methods and the clipboard (T9.4.7, T9.4.8)
//!
//! An input method's composition is never an edit: it is drawn in the
//! story through [`crate::tool::Preview::text`] and the caret follows it;
//! its commit arrives as typing. Cut and paste replace the selection with
//! one step each ([`EditCommand::CutText`], [`EditCommand::PasteText`]);
//! the copy itself is taken by the application ([`TextTool::copy_selection`],
//! [`crate::text_clip`]).
//!
//! # Layouts
//!
//! The tool lays stories out itself, as the walker does
//! (`text::lay_story`), and keeps them in a cache keyed by the
//! document's epoch: any committed edit (undo included) drops them all. A
//! story on a path keeps its fit, so the caret, hit tests and highlight
//! follow the drawn text ([`CaretMap::on_path`]).
//!
//! # Indents on a path (T9.5.5)
//!
//! While a caret is in a story on a path, two handles sit on the path where
//! its text may start and end (the left and right indents, in the text's
//! direction). Dragging one slides it along the path, never past the other,
//! and its release is one [`crate::text_path_edit::TextPathEdit::Indents`]
//! step. The path itself is an ordinary child node, so the shape editor
//! still edits it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use kurbo::Affine;
use xarast_doc::{
    AttrSlot, AttrValue, Document, NodeId, NodeKind, StoryText, TextCursor, TextLayout,
    TextStoryNode,
};
use xarast_geom::{Matrix, Mp, Vector};

use crate::edit::{SelectMode, ToolId};
use crate::fonts::FontService;
use crate::geometry::{DocPoint, DocRect};
use crate::ops::EditCommand;
use crate::text_clip::{StyledText, TextClipOp};
use crate::text_edit::{Caret, CaretMap, CaretMotion};
use crate::text_infobar::{self, Values};
use crate::tool::{
    CursorKind, GestureEvent, Infobar, InfobarField, InfobarValue, InteractionState, OverlayShape,
    Preedit, TextInput, TextInputKind, TextKey, TextNav, TextPreview, TextRuler, Tool, ToolAction,
    ToolCtx, ToolView,
};

/// Typing keys closer together than this, in milliseconds, make one undo
/// step (`phase-09` W9.4).
pub const TYPING_BURST_MS: u64 = 500;

/// Burst numbers: process-wide, so no two bursts of any document share one.
static NEXT_BURST: AtomicU64 = AtomicU64::new(1);

fn next_burst() -> u64 {
    NEXT_BURST.fetch_add(1, Ordering::Relaxed)
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum BurstKind {
    Typing,
    Deleting,
}

/// The typing burst in progress: what the next key must match to join it.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
struct Burst {
    id: u64,
    kind: BurstKind,
    /// The story it edits; `None` until the story it creates exists.
    story: Option<NodeId>,
    /// Where it left the caret.
    caret: usize,
    /// When its last key was pressed.
    last_ms: u64,
    /// The document's epoch right after its last edit applied; `None`
    /// until then (and for good when the edit failed).
    epoch: Option<u64>,
}

impl Burst {
    /// The undo-step key its commands coalesce on.
    fn key(&self) -> xarast_doc::CoalesceKey {
        xarast_doc::CoalesceKey {
            gesture: self.id,
            kind: match self.kind {
                BurstKind::Typing => crate::ops::TYPING_KIND,
                BurstKind::Deleting => crate::ops::TEXT_DELETE_KIND,
            },
        }
    }
}

use crate::text_clip::typed;

/// A caret and selection in a story: the text between `anchor` and `head`.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct TextSelection {
    /// The `TextStory` node.
    pub story: NodeId,
    /// Where the selection started.
    pub anchor: Caret,
    /// Where the caret is.
    pub head: Caret,
}

impl TextSelection {
    /// Whether nothing is selected.
    #[must_use]
    pub fn is_caret(&self) -> bool {
        self.anchor.byte == self.head.byte
    }

    /// The selected byte range, in order.
    #[must_use]
    pub fn range(&self) -> std::ops::Range<usize> {
        self.anchor.byte.min(self.head.byte)..self.anchor.byte.max(self.head.byte)
    }

    /// The document model's view of it: positions as line and item.
    /// `None` when the story is gone.
    #[must_use]
    pub fn cursor(&self, doc: &Document) -> Option<TextCursor> {
        let mut stack = xarast_doc::attr::resolve_inherited(&doc.tree, self.story, &doc.defaults);
        let st = StoryText::collect(&doc.tree, self.story, &mut stack, &mut |_, a| {
            Arc::new(a.value.clone())
        })?;
        Some(TextCursor {
            story: self.story,
            anchor: st.pos_of(self.anchor.byte)?,
            head: st.pos_of(self.head.byte)?,
        })
    }
}

/// What the text tool is editing.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum TextEditing {
    /// A caret in an existing story.
    Story(TextSelection),
    /// A caret where typing will create a story.
    Pending {
        /// The new story's origin: the first baseline's left end.
        at: DocPoint,
        /// The column width a drag asked for; a point story when `None`.
        column: Option<Mp>,
    },
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
struct Editing {
    state: TextEditing,
    /// The x a run of vertical moves keeps, in story space.
    goal_x: Option<Mp>,
}

#[derive(Copy, Clone, PartialEq, Debug)]
enum Drag {
    /// Selecting text by dragging over a story.
    Select {
        story: NodeId,
        anchor: Caret,
        before: Option<Editing>,
    },
    /// Dragging out a column on empty canvas.
    Column {
        from: DocPoint,
        to: DocPoint,
        before: Option<Editing>,
    },
    /// Dragging an indent handle of a story on a path: the indents as
    /// the drag has them so far, in story space.
    Indent {
        story: NodeId,
        /// The left handle (`false`: the right one).
        left_end: bool,
        left: Mp,
        right: Mp,
    },
}

/// A story laid out for editing, with its matrix.
#[derive(Debug)]
struct StoryView {
    map: CaretMap,
    /// The story's text and resolved attributes, as laid out.
    st: StoryText,
    /// The story node, as laid out.
    node: TextStoryNode,
    /// Story space to document space.
    xf: Affine,
    /// Document space to story space.
    inv: Affine,
}

impl StoryView {
    /// Lays `story` out; with `splice`, as if its text were typed at its
    /// offset (an input method's composition, as the walker draws it).
    fn build(
        doc: &Document,
        story: NodeId,
        fonts: &FontService,
        splice: Option<(usize, &str)>,
    ) -> Option<StoryView> {
        let Some(NodeKind::TextStory(node)) = doc.tree.kind(story) else {
            return None;
        };
        let mut stack = xarast_doc::attr::resolve_inherited(&doc.tree, story, &doc.defaults);
        let mut st = StoryText::collect(&doc.tree, story, &mut stack, &mut |_, a| {
            Arc::new(a.value.clone())
        })?;
        if let Some((at, text)) = splice {
            st = crate::text::splice_text(&st, at, text);
        }
        // The walker's own layout: on a path, the path's column and the fit.
        let (_, layout, fit) = crate::text::lay_story(fonts, &doc.tree, &st, node);
        let node = (**node).clone();
        let text = crate::text::layout_text(&st).to_owned();
        let xf = node.transform.to_affine();
        let inv = if xf.determinant().abs() > f64::EPSILON {
            xf.inverse()
        } else {
            Affine::IDENTITY
        };
        let map = match fit {
            Some(fit) => CaretMap::on_path(text, layout, fit),
            None => CaretMap::new(text, layout),
        };
        Some(StoryView {
            map,
            st,
            node,
            xf,
            inv,
        })
    }

    fn to_doc(&self, p: kurbo::Point) -> DocPoint {
        let p = self.xf * p;
        DocPoint::from_f64_round(p.x, p.y)
    }

    fn to_story(&self, p: DocPoint) -> kurbo::Point {
        self.inv * kurbo::Point::new(p.x.to_f64(), p.y.to_f64())
    }

    /// The caret a document point hits, and whether it is on the story
    /// (within `tol` document units of a line or, on a path, of a fitted
    /// cluster).
    fn hit(&self, p: DocPoint, tol: f64) -> (Caret, bool) {
        let (caret, dist) = self.map.hit_point(self.to_story(p));
        (caret, dist <= tol * self.story_scale())
    }

    /// Story units per document unit: how far a device tolerance reaches
    /// in the story.
    fn story_scale(&self) -> f64 {
        let d = self.inv.determinant().abs().sqrt();
        if d.is_finite() && d > 0.0 { d } else { 1.0 }
    }
}

#[derive(Debug, Default)]
struct Cache {
    epoch: Option<u64>,
    stories: HashMap<NodeId, Option<Arc<StoryView>>>,
    /// The story laid out with the composition in it, for the preview
    /// it was built for.
    composing: Option<(TextPreview, Option<Arc<StoryView>>)>,
}

/// How far, in device pixels, a click may miss a line box and still land
/// in the story.
const STORY_HIT_PX: f64 = 4.0;

/// The size a pending caret is drawn at when no current font size is set:
/// the model's default (`text.md`, 16 pt).
const DEFAULT_SIZE: Mp = Mp::new(16_000);

/// A drag narrower than this, in device pixels, makes a point story, not a
/// column.
const MIN_COLUMN_PX: f64 = 8.0;

/// The text tool.
#[derive(Debug, Default)]
pub struct TextTool {
    /// Fonts to lay out with; the process's shared service when `None`.
    fonts: Option<Arc<FontService>>,
    cache: Mutex<Cache>,
    editing: Option<Editing>,
    drag: Option<Drag>,
    /// Arrow keys move in logical rather than visual order (the
    /// preference `phase-09` names; visual is the default).
    pub logical_arrows: bool,
    /// Counts caret changes, so the interface restarts the blink.
    moved: u64,
    /// The typing burst the next key may join.
    burst: Option<Burst>,
    /// Attributes chosen at a caret, for the text typed there next: the
    /// character attributes in a story, any at a pending caret.
    pending: Vec<AttrValue>,
    /// The kind of tab stop a click on the text ruler adds (`kind & 3`).
    tab_kind: u8,
    /// The font families the infobar last offered: what a chosen index
    /// refers to.
    families: Mutex<Option<Arc<[Arc<str>]>>>,
    /// The input method's composition at the caret, if one is in progress
    /// (T9.4.7), and where it shows in a story (`None` at a pending caret,
    /// where there is no story to show it in yet).
    composing: Option<(Preedit, Option<TextPreview>)>,
    /// A paste at a pending caret creates a story: the caret goes into it
    /// at this offset once it exists.
    adopt: Option<usize>,
    /// An edit just emitted takes all of a story's text: where the caret
    /// goes if that removes the story ([`Emptying`]).
    emptying: Option<Emptying>,
    /// The story the tool has edited since the caret went into it: the
    /// only one leaving it may remove (XARA-T-0295). A story the user only
    /// entered (an imported empty one) is never touched.
    edited: Option<NodeId>,
    /// The story and undo-step key of the last typing or deleting burst
    /// that applied: a removal on leaving merges into that step when it is
    /// still the last one.
    last_burst: Option<(NodeId, xarast_doc::CoalesceKey)>,
}

/// A story an edit is about to empty, which the edit then removes
/// (`text.md`, "Removing an emptied story"): the caret becomes a pending
/// one where the story began, keeping its style, so typing on makes a new
/// story that looks like the old one.
#[derive(Clone, Debug, PartialEq)]
struct Emptying {
    story: NodeId,
    /// The pending caret: the story's origin and column width; `None` for
    /// a story on a path (its path stays, as a shape, and editing ends).
    pending: Option<(DocPoint, Option<Mp>)>,
    /// The attributes of the story's first character that differ from
    /// what a new story would get.
    style: Vec<AttrValue>,
}

impl Emptying {
    /// `Some` when deleting `range` of the story `v` (replacing it with an
    /// empty `text`) leaves it without characters: only paragraph breaks
    /// and tabs ([`xarast_doc::holds_no_characters`]).
    fn of(
        story: NodeId,
        v: &StoryView,
        range: &std::ops::Range<usize>,
        text: &str,
        doc: &Document,
        current: &[AttrValue],
    ) -> Option<Emptying> {
        let all = v.map.text();
        if !text.is_empty()
            || range.is_empty()
            || range.end > all.len()
            || !all.is_char_boundary(range.start)
            || !all.is_char_boundary(range.end)
            || !xarast_doc::holds_no_characters(&all[..range.start])
            || !xarast_doc::holds_no_characters(&all[range.end..])
        {
            return None;
        }
        let pending = match v.node.layout {
            TextLayout::OnPath { .. } => None,
            TextLayout::InColumn { width, .. } => Some(width),
            TextLayout::AtPoint => Some(Mp::ZERO),
        }
        .map(|w| {
            let m = v.node.transform;
            (DocPoint::new(m.e, m.f), (w > Mp::ZERO).then_some(w))
        });
        let style =
            v.st.run_at(0)
                .and_then(|r| v.st.runs.get(r))
                .map(|run| {
                    ALL_TEXT_SLOTS
                        .into_iter()
                        .chain(crate::text_clip::PAINT_SLOTS)
                        .filter_map(|slot| {
                            let has = run.attrs.get(slot);
                            let new = current
                                .iter()
                                .find(|c| c.slot() == Some(slot))
                                .unwrap_or_else(|| &**doc.defaults.get(slot));
                            (has != new).then(|| has.clone())
                        })
                        .collect()
                })
                .unwrap_or_default();
        Some(Emptying {
            story,
            pending,
            style,
        })
    }
}

impl TextTool {
    /// A text tool laying stories out with `fonts` rather than the process's
    /// shared service: for tests with pinned fonts.
    #[must_use]
    pub fn with_fonts(fonts: Arc<FontService>) -> TextTool {
        TextTool {
            fonts: Some(fonts),
            ..TextTool::default()
        }
    }

    fn fonts(&self) -> Arc<FontService> {
        self.fonts.clone().unwrap_or_else(crate::fonts::shared)
    }

    /// The fonts `doc`'s stories are laid out with, as the walker lays
    /// them out: [`TextTool::fonts`] plus the faces the document embeds.
    fn doc_fonts(&self, doc: &Document) -> Arc<FontService> {
        crate::fonts::for_document(&self.fonts(), doc)
    }

    fn cache(&self) -> MutexGuard<'_, Cache> {
        self.cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The story laid out, from the cache when the document has not changed.
    fn view(&self, doc: &Document, story: NodeId) -> Option<Arc<StoryView>> {
        let mut cache = self.cache();
        if cache.epoch != Some(doc.epoch.0) {
            cache.stories.clear();
            cache.composing = None;
            cache.epoch = Some(doc.epoch.0);
        }
        if let Some(v) = cache.stories.get(&story) {
            return v.clone();
        }
        let v = if doc.tree.contains(story) && doc.tree.is_reachable(story) {
            StoryView::build(doc, story, &self.doc_fonts(doc), None).map(Arc::new)
        } else {
            None
        };
        cache.stories.insert(story, v.clone());
        v
    }

    /// The story laid out with the input method's composition in it.
    fn composing_view(&self, doc: &Document, p: &TextPreview) -> Option<Arc<StoryView>> {
        // Drops a stale cache first.
        let _ = self.view(doc, p.story);
        let mut cache = self.cache();
        if let Some((key, v)) = &cache.composing
            && key == p
        {
            return v.clone();
        }
        let v = StoryView::build(doc, p.story, &self.doc_fonts(doc), Some((p.at, &p.text)))
            .map(Arc::new);
        cache.composing = Some((p.clone(), v.clone()));
        v
    }

    /// The story on a path the caret is in, its view, and its indents.
    fn path_story(&self, doc: &Document) -> Option<(NodeId, Arc<StoryView>, Mp, Mp)> {
        let Some(Editing {
            state: TextEditing::Story(sel),
            ..
        }) = self.editing
        else {
            return None;
        };
        let v = self.view(doc, sel.story)?;
        v.map.path_fit()?;
        match v.node.layout {
            TextLayout::OnPath {
                left_indent,
                right_indent,
                ..
            } => Some((sel.story, v, left_indent, right_indent)),
            _ => None,
        }
    }

    /// Where the indent handles of `v` sit for the indents `left` and
    /// `right`, in document space: left, then right.
    fn indent_handles(v: &StoryView, left: Mp, right: Mp) -> Option<[DocPoint; 2]> {
        let path = v.map.path_fit()?.path();
        let start = path.at(left.to_f64()).0;
        let end = path.at(path.length() - right.to_f64()).0;
        Some([v.to_doc(start), v.to_doc(end)])
    }

    /// An indent handle under `at`: the drag it starts.
    fn indent_at(&self, cx: &ToolCtx<'_>, at: DocPoint) -> Option<Drag> {
        let (story, v, left, right) = self.path_story(cx.doc)?;
        let [l, r] = Self::indent_handles(&v, left, right)?;
        let near =
            |h| crate::tool::near_on_screen(cx.viewport, h, at, crate::tool::HANDLE_TOLERANCE_PX);
        let left_end = if near(l) {
            true
        } else if near(r) {
            false
        } else {
            return None;
        };
        Some(Drag::Indent {
            story,
            left_end,
            left,
            right,
        })
    }

    /// The topmost editable story under a document point, and the caret
    /// the point hits in it.
    fn story_at(&self, cx: &ToolCtx<'_>, at: DocPoint) -> Option<(NodeId, Arc<StoryView>, Caret)> {
        let tol = STORY_HIT_PX * cx.device_px();
        let mut found = None;
        for id in editable_stories(cx.doc) {
            let Some(v) = self.view(cx.doc, id) else {
                continue;
            };
            let (caret, on) = v.hit(at, tol);
            if on {
                found = Some((id, v, caret));
            }
        }
        found
    }

    /// The text being edited, if any.
    #[must_use]
    pub fn editing(&self) -> Option<TextEditing> {
        self.editing.map(|e| e.state)
    }

    fn set(&mut self, state: Option<TextEditing>, goal_x: Option<Mp>, cx: &mut ToolCtx<'_>) {
        let new = state.map(|state| Editing { state, goal_x });
        let story_of = |e: Option<Editing>| match e {
            Some(Editing {
                state: TextEditing::Story(s),
                ..
            }) => Some(s.story),
            _ => None,
        };
        if let Some(from) = story_of(self.editing)
            && story_of(new) != Some(from)
        {
            self.leave(from, cx);
        }
        // Anything that sets the caret ends a typing burst; typing starts
        // its own again right after.
        self.burst = None;
        self.adopt = None;
        if new != self.editing {
            self.moved = self.moved.wrapping_add(1);
            // A pending style belongs to the caret it was chosen at.
            if new.map(|e| e.state) != self.editing.map(|e| e.state) {
                self.pending.clear();
                // So does a composition: it no longer shows anywhere (its
                // commit, if it comes, types at the new caret).
                self.end_composition(cx);
            }
        }
        self.editing = new;
        cx.requests.overlay_changed = true;
    }

    /// Editing `story` ends (Esc, a tool switch, a click elsewhere): a story
    /// the tool edited that now holds no characters, only paragraph breaks
    /// and tabs, is removed, as the original does when editing ends
    /// (`text.md`, "Removing an emptied story"; tools invariant 11). The
    /// removal merges into the burst that left the story so when that is
    /// still the last undo step; otherwise it is a "Delete Text" step of
    /// its own.
    fn leave(&mut self, story: NodeId, cx: &mut ToolCtx<'_>) {
        let edited = self.edited.take() == Some(story);
        let joins = self
            .last_burst
            .take()
            .filter(|(s, _)| *s == story)
            .map(|(_, key)| key);
        if edited
            && cx.doc.tree.is_reachable(story)
            && is_editable(&cx.doc.tree, story)
            && xarast_doc::is_story_empty(cx.doc, story) == Some(true)
        {
            cx.commands
                .emit(EditCommand::RemoveEmptyStory { story, joins });
        }
    }

    fn select_story(&mut self, story: NodeId, anchor: Caret, head: Caret, cx: &mut ToolCtx<'_>) {
        let was = match self.editing {
            Some(Editing {
                state: TextEditing::Story(s),
                ..
            }) => Some(s.story),
            _ => None,
        };
        if was != Some(story) || !cx.edit.is_selected(story) || cx.edit.selection_len() != 1 {
            cx.requests.select(vec![story], SelectMode::Replace);
        }
        self.set(
            Some(TextEditing::Story(TextSelection {
                story,
                anchor,
                head,
            })),
            None,
            cx,
        );
    }

    /// The selection in force, checked against the document: a story that
    /// is gone ends the editing, and offsets past the text are clamped.
    fn current(&mut self, doc: &Document) -> Option<(TextSelection, Arc<StoryView>)> {
        let Some(Editing {
            state: TextEditing::Story(s),
            ..
        }) = self.editing
        else {
            return None;
        };
        let Some(v) = self.view(doc, s.story) else {
            self.editing = None;
            return None;
        };
        let clamp = |c: Caret| v.map.snap(c);
        Some((
            TextSelection {
                anchor: clamp(s.anchor),
                head: clamp(s.head),
                ..s
            },
            v,
        ))
    }

    fn click(&mut self, at: DocPoint, count: u8, cx: &mut ToolCtx<'_>) {
        let Some((story, v, hit)) = self.story_at(cx, at) else {
            self.pending(at, None, cx);
            return;
        };
        match count {
            1 => {
                let anchor = match self.current(cx.doc) {
                    Some((s, _)) if cx.modifiers.adjust && s.story == story => s.anchor,
                    _ => hit,
                };
                self.select_story(story, anchor, hit, cx);
            }
            2 => {
                let w = v.map.word_at(hit.byte);
                self.select_story(story, Caret::at(w.start), Caret::after(w.end), cx);
            }
            _ => {
                let line = v.map.line_of(hit);
                let r = v.map.line_range(line);
                self.select_story(story, Caret::at(r.start), Caret::after(r.end), cx);
            }
        }
    }

    fn pending(&mut self, at: DocPoint, column: Option<Mp>, cx: &mut ToolCtx<'_>) {
        if !cx.edit.is_selection_empty() {
            cx.requests.select(Vec::new(), SelectMode::Replace);
        }
        self.set(Some(TextEditing::Pending { at, column }), None, cx);
    }

    /// The size a new story's caret is drawn at: the pending style's font
    /// size, else the current one.
    fn pending_size(&self, edit: &crate::edit::EditState) -> Mp {
        Self::size_in(&self.pending, edit)
    }

    /// The font size `first`, else the current attributes, give.
    fn size_in(first: &[AttrValue], edit: &crate::edit::EditState) -> Mp {
        first
            .iter()
            .chain(edit.current.values())
            .find_map(|v| match v {
                AttrValue::FontSize(s) if s.raw() > 0 => Some(*s),
                _ => None,
            })
            .unwrap_or(DEFAULT_SIZE)
    }

    /// A typing key: characters, Enter, Backspace, Delete.
    fn input(&mut self, input: &TextInput, cx: &mut ToolCtx<'_>) -> bool {
        let Some(editing) = self.editing else {
            return false;
        };
        // A commit arrives as typing: the composition it ends goes.
        self.end_composition(cx);
        self.adopt = None;
        self.emptying = None;
        if let TextEditing::Pending { at, column } = editing.state {
            if let TextInputKind::Insert(t) = &input.kind {
                self.start_story(at, column, typed(t), input.time_ms, cx);
            }
            // Nothing to delete yet.
            return true;
        }
        let Some((sel, v)) = self.current(cx.doc) else {
            return true;
        };
        let map = &v.map;
        let head = sel.head.byte;
        let (kind, range, text) = match &input.kind {
            TextInputKind::Insert(t) => (BurstKind::Typing, sel.range(), typed(t)),
            _ if !sel.is_caret() => (BurstKind::Deleting, sel.range(), String::new()),
            TextInputKind::Backspace { word } => {
                let from = if *word {
                    map.move_caret(sel.head, CaretMotion::Word, false, None)
                        .byte
                        .min(head)
                } else {
                    xarast_text::prev_grapheme(map.text(), head)
                };
                (BurstKind::Deleting, from..head, String::new())
            }
            TextInputKind::Delete { word } => {
                let to = if *word {
                    map.move_caret(sel.head, CaretMotion::Word, true, None)
                        .byte
                        .max(head)
                } else {
                    xarast_text::next_grapheme(map.text(), head)
                };
                (BurstKind::Deleting, head..to, String::new())
            }
        };
        if range.is_empty() && text.is_empty() {
            return true;
        }
        let joins = self.burst.is_some_and(|b| {
            b.kind == kind
                && b.story == Some(sel.story)
                && sel.is_caret()
                && b.caret == head
                && b.epoch == Some(cx.doc.epoch.0)
                && input.time_ms >= b.last_ms
                && input.time_ms - b.last_ms < TYPING_BURST_MS
        });
        let id = match self.burst {
            Some(b) if joins => b.id,
            _ => next_burst(),
        };
        let caret = range.start + text.len();
        // The style chosen at the caret styles what is typed there.
        let style = if kind == BurstKind::Typing && !text.is_empty() {
            std::mem::take(&mut self.pending)
        } else {
            Vec::new()
        };
        let typed_range = range.start..caret;
        let emptying = Emptying::of(
            sel.story,
            &v,
            &range,
            &text,
            cx.doc,
            cx.edit.current.values(),
        );
        self.edited = Some(sel.story);
        cx.commands.emit(match kind {
            BurstKind::Typing => EditCommand::TypeText {
                story: sel.story,
                replace: range,
                text,
                burst: id,
            },
            BurstKind::Deleting => EditCommand::DeleteText {
                story: sel.story,
                range,
                burst: id,
            },
        });
        if !style.is_empty() {
            cx.commands.emit(EditCommand::SetTextAttr {
                story: sel.story,
                edits: style
                    .into_iter()
                    .map(|v| (typed_range.clone(), v))
                    .collect(),
                burst: Some(id),
            });
        }
        self.set(
            Some(TextEditing::Story(TextSelection {
                story: sel.story,
                anchor: Caret::at(caret),
                head: Caret::at(caret),
            })),
            None,
            cx,
        );
        self.burst = Some(Burst {
            id,
            kind,
            story: Some(sel.story),
            caret,
            last_ms: input.time_ms,
            epoch: None,
        });
        self.emptying = emptying;
        true
    }

    /// The first characters typed at a pending caret: a new story holding
    /// them, on the active layer, with the current attributes.
    fn start_story(
        &mut self,
        at: DocPoint,
        column: Option<Mp>,
        text: String,
        time_ms: u64,
        cx: &mut ToolCtx<'_>,
    ) {
        let Some(layer) = cx.edit.active_layer() else {
            return;
        };
        if text.is_empty() {
            return;
        }
        let id = next_burst();
        let caret = text.len();
        cx.commands.emit(EditCommand::CreateText {
            layer,
            story: new_story_node(at, column),
            attrs: merged(cx.edit.current.values(), &std::mem::take(&mut self.pending)),
            text,
            burst: id,
        });
        self.burst = Some(Burst {
            id,
            kind: BurstKind::Typing,
            story: None,
            caret,
            last_ms: time_ms,
            epoch: None,
        });
    }

    /// Ends the composition in progress, if any, and drops its preview.
    fn end_composition(&mut self, cx: &mut ToolCtx<'_>) {
        if self.composing.take().is_some() || cx.preview.text.is_some() {
            cx.preview.text = None;
            cx.requests.overlay_changed = true;
        }
    }

    /// The input method's composition changed (T9.4.7). In a story it is
    /// drawn in place, at the start of the selection (which its commit
    /// replaces), through the preview; at a pending caret there is no
    /// story to draw it in, and only the candidate window follows the
    /// caret.
    fn preedit(&mut self, preedit: Option<Preedit>, cx: &mut ToolCtx<'_>) -> bool {
        let Some(editing) = self.editing else {
            return false;
        };
        let Some(p) = preedit.filter(|p| !p.text.is_empty()) else {
            self.end_composition(cx);
            return true;
        };
        let shown = match editing.state {
            TextEditing::Story(_) => self.current(cx.doc).map(|(sel, _)| TextPreview {
                story: sel.story,
                at: sel.range().start,
                text: p.text.clone(),
            }),
            TextEditing::Pending { .. } => None,
        };
        cx.preview.text.clone_from(&shown);
        self.composing = Some((p, shown));
        // A composition is not typing: the next commit starts its own burst.
        self.burst = None;
        cx.requests.overlay_changed = true;
        true
    }

    /// Cut or paste at the caret (T9.4.8). The copy is not the tool's: the
    /// application takes it with [`TextTool::copy_selection`].
    fn clipboard(&mut self, op: TextClipOp, cx: &mut ToolCtx<'_>) -> bool {
        let Some(editing) = self.editing else {
            return false;
        };
        self.end_composition(cx);
        self.emptying = None;
        match (editing.state, op) {
            (TextEditing::Pending { .. }, TextClipOp::Cut) => {}
            (TextEditing::Pending { at, column }, TextClipOp::Paste(text)) => {
                let Some(layer) = cx.edit.active_layer() else {
                    return true;
                };
                if text.is_empty() {
                    return true;
                }
                let caret = text.text.len();
                cx.commands.emit(EditCommand::PasteText {
                    target: crate::ops::PasteTarget::New {
                        layer,
                        story: new_story_node(at, column),
                        attrs: merged(cx.edit.current.values(), &std::mem::take(&mut self.pending)),
                    },
                    text,
                });
                self.burst = None;
                self.adopt = Some(caret);
            }
            (TextEditing::Story(_), op) => {
                let Some((sel, v)) = self.current(cx.doc) else {
                    return true;
                };
                let range = sel.range();
                let emptied = |text: &str| {
                    Emptying::of(
                        sel.story,
                        &v,
                        &range,
                        text,
                        cx.doc,
                        cx.edit.current.values(),
                    )
                };
                let emptying = match &op {
                    TextClipOp::Cut => emptied(""),
                    TextClipOp::Paste(t) => emptied(&t.text),
                };
                self.edited = Some(sel.story);
                let caret = match op {
                    TextClipOp::Cut if sel.is_caret() => return true,
                    TextClipOp::Cut => {
                        cx.commands.emit(EditCommand::CutText {
                            story: sel.story,
                            range: range.clone(),
                        });
                        range.start
                    }
                    TextClipOp::Paste(text) => {
                        if text.is_empty() && sel.is_caret() {
                            return true;
                        }
                        let caret = range.start + text.text.len();
                        cx.commands.emit(EditCommand::PasteText {
                            target: crate::ops::PasteTarget::Story {
                                story: sel.story,
                                replace: range,
                            },
                            text,
                        });
                        caret
                    }
                };
                self.set(
                    Some(TextEditing::Story(TextSelection {
                        story: sel.story,
                        anchor: Caret::at(caret),
                        head: Caret::at(caret),
                    })),
                    None,
                    cx,
                );
                self.emptying = emptying;
            }
        }
        true
    }

    /// The styled copy of the selected text (T9.4.8), if text is selected.
    #[must_use]
    pub fn copy_selection(&self, doc: &Document) -> Option<StyledText> {
        let Some(TextEditing::Story(sel)) = self.editing() else {
            return None;
        };
        if sel.is_caret() {
            return None;
        }
        let v = self.view(doc, sel.story)?;
        let (a, b) = (v.map.snap(sel.anchor).byte, v.map.snap(sel.head).byte);
        let clip = crate::text_clip::copy_range(&v.st, a.min(b)..a.max(b));
        (!clip.is_empty()).then_some(clip)
    }

    /// The caret while composing in a story: at the composition's cursor,
    /// in the story laid out with it; and where the composition starts.
    fn composing_caret(&self, doc: &Document) -> Option<(Arc<StoryView>, Caret, usize)> {
        let (p, Some(shown)) = self.composing.as_ref()? else {
            return None;
        };
        let v = self.composing_view(doc, shown)?;
        let at = shown.at + p.cursor.map_or(p.text.len(), |c| c.1.min(p.text.len()));
        Some((v, Caret::at(at), shown.at))
    }

    /// The composition drawn over its story: underlined, its selected
    /// segment highlighted, the caret at its cursor (none when the input
    /// method hides it).
    fn composition_overlay(&self, doc: &Document, out: &mut Vec<OverlayShape>) {
        let Some((v, caret, at)) = self.composing_caret(doc) else {
            return;
        };
        let Some((p, _)) = &self.composing else {
            return;
        };
        let map = &v.map;
        for q in map.selection_quads(at, at + p.text.len()) {
            out.push(OverlayShape::Polyline {
                points: vec![v.to_doc(q[0]), v.to_doc(q[1])],
                closed: false,
                dashed: false,
            });
        }
        if let Some((a, b)) = p.cursor.filter(|(a, b)| a < b) {
            for q in map.selection_quads(at + a, at + b) {
                out.push(OverlayShape::Highlight {
                    corners: q.map(|pt| v.to_doc(pt)),
                });
            }
        }
        if p.cursor.is_some() {
            let (primary, _) = map.caret_segments(caret);
            out.push(OverlayShape::Caret {
                from: v.to_doc(primary.bottom),
                to: v.to_doc(primary.top),
                primary: true,
                moved: self.moved,
            });
        }
    }

    fn navigate(&mut self, nav: TextNav, cx: &mut ToolCtx<'_>) -> bool {
        let Some((sel, v)) = self.current(cx.doc) else {
            // A pending caret has nowhere to go; the keys are still the
            // text's, not the view's.
            return self.editing.is_some();
        };
        let map = &v.map;
        let base_rtl = map
            .layout()
            .lines
            .get(map.line_of(sel.head))
            .is_some_and(|l| l.base_rtl);
        let right = matches!(nav.key, TextKey::Right);
        // Arrow keys in logical order go backwards in a right-to-left
        // paragraph when they point right.
        let logical_forward = right != base_rtl;
        let goal = self.editing.and_then(|e| e.goal_x);
        let page = Mp::from_f64_round(
            f64::from(cx.viewport.size().height) * cx.device_px() * v.story_scale(),
        );
        let (motion, forward) = match nav.key {
            TextKey::Left | TextKey::Right if nav.word => (CaretMotion::Word, logical_forward),
            TextKey::Left | TextKey::Right if self.logical_arrows => {
                (CaretMotion::Character { visual: false }, logical_forward)
            }
            TextKey::Left | TextKey::Right => (CaretMotion::Character { visual: true }, right),
            TextKey::Up => (CaretMotion::Line, false),
            TextKey::Down => (CaretMotion::Line, true),
            TextKey::Home if nav.word => (CaretMotion::StoryStart, false),
            TextKey::End if nav.word => (CaretMotion::StoryEnd, true),
            TextKey::Home => (CaretMotion::LineStart, false),
            TextKey::End => (CaretMotion::LineEnd, true),
            TextKey::PageUp => (CaretMotion::Page { height: page }, false),
            TextKey::PageDown => (CaretMotion::Page { height: page }, true),
        };
        let vertical = matches!(motion, CaretMotion::Line | CaretMotion::Page { .. });
        let head =
            if !nav.extend && !sel.is_caret() && matches!(motion, CaretMotion::Character { .. }) {
                // A selection collapses to the end the arrow points at.
                let r = sel.range();
                if logical_forward {
                    Caret::after(r.end)
                } else {
                    Caret::at(r.start)
                }
            } else {
                map.move_caret(sel.head, motion, forward, goal)
            };
        let goal_x = if vertical {
            Some(goal.unwrap_or_else(|| map.caret_x(sel.head)))
        } else {
            None
        };
        let anchor = if nav.extend { sel.anchor } else { head };
        self.set(
            Some(TextEditing::Story(TextSelection {
                story: sel.story,
                anchor,
                head,
            })),
            goal_x,
            cx,
        );
        true
    }
}

/// Where the text infobar's edits go.
enum Target {
    /// The selection or caret in a story.
    Story(TextSelection, Arc<StoryView>),
    /// A caret where typing will create a story.
    Pending,
    /// No caret: the selected stories, whole.
    Stories(Vec<(NodeId, Arc<StoryView>)>),
    /// Nothing to apply to: the current attributes.
    Current,
}

/// Every text slot, for merging the values of several stories.
const ALL_TEXT_SLOTS: [AttrSlot; 16] = [
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

/// `base` with every value of `over` replacing the one of its slot.
/// A new story at a pending caret: a point story, or a column of `column`
/// width, its first baseline's left end at `at`.
fn new_story_node(at: DocPoint, column: Option<Mp>) -> Box<TextStoryNode> {
    let layout = match column {
        Some(width) => TextLayout::InColumn {
            width,
            word_wrap: true,
        },
        None => TextLayout::AtPoint,
    };
    Box::new(TextStoryNode {
        transform: Matrix::translate(Vector::new(at.x, at.y)),
        layout,
        ..TextStoryNode::default()
    })
}

fn merged(base: &[AttrValue], over: &[AttrValue]) -> Vec<AttrValue> {
    let mut out = base.to_vec();
    for v in over {
        set_slot(&mut out, v.clone());
    }
    out
}

/// Puts `v` into `list`, replacing the value of its slot.
fn set_slot(list: &mut Vec<AttrValue>, v: AttrValue) {
    list.retain(|o| o.slot() != v.slot());
    list.push(v);
}

/// The attribute an edit sets when it has no text to go through: a
/// pending caret's style or the current attributes. Tab stops are not
/// edited there (no ruler shows).
fn plain_edit(
    field: InfobarField,
    value: InfobarValue,
    shown: &Values,
    families: &[Arc<str>],
) -> Option<AttrValue> {
    if let (InfobarField::TextFeature(tag), InfobarValue::Toggle(on)) = (field, value) {
        let list = match shown.one(AttrSlot::TxtFeatures) {
            Some(AttrValue::FontFeatures(f)) => Arc::clone(f),
            _ => Arc::from(Vec::new()),
        };
        return Some(AttrValue::FontFeatures(text_infobar::with_feature(
            &list, tag, on,
        )));
    }
    let v = text_infobar::value_for(field, value, shown, families)?;
    (shown.one(v.slot()?) != Some(&v)).then_some(v)
}

/// The edits a field sets on a byte range of a story: nothing when the
/// range already has the value. `tabs` are the stops of the range's
/// paragraph, `tab_kind` the kind a ruler click adds.
#[allow(clippy::too_many_arguments)]
fn story_edits(
    field: InfobarField,
    value: InfobarValue,
    st: &StoryText,
    range: &std::ops::Range<usize>,
    shown: &Values,
    families: &[Arc<str>],
    tabs: &[xarast_doc::TabStop],
    tab_kind: u8,
) -> Vec<(std::ops::Range<usize>, AttrValue)> {
    if let (InfobarField::TextFeature(tag), InfobarValue::Toggle(on)) = (field, value) {
        return text_infobar::feature_edits(st, range, tag, on);
    }
    if let Some(t) = text_infobar::tabs_after(field, value, tabs, tab_kind) {
        return vec![(range.clone(), AttrValue::Ruler(t))];
    }
    match text_infobar::value_for(field, value, shown, families) {
        Some(v) if v.slot().is_some_and(|s| shown.one(s) != Some(&v)) => {
            vec![(range.clone(), v)]
        }
        _ => Vec::new(),
    }
}

impl TextTool {
    /// The font families the chooser offers: the database's, once it is
    /// enumerated (an empty list until then). Remembered, so that a chosen
    /// index finds the family it was shown for.
    fn families(&self) -> Arc<[Arc<str>]> {
        let fonts = self.fonts();
        let list = if fonts.is_loaded() {
            fonts.db().families()
        } else {
            fonts.start_loading();
            Arc::from(Vec::new())
        };
        *self
            .families
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&list));
        list
    }

    /// Where the infobar's edits go now.
    fn target(&self, doc: &Document, edit: &crate::edit::EditState) -> Target {
        match self.editing.map(|e| e.state) {
            Some(TextEditing::Story(s)) => match self.view(doc, s.story) {
                Some(v) => {
                    let clamp = |c: Caret| v.map.snap(c);
                    let sel = TextSelection {
                        anchor: clamp(s.anchor),
                        head: clamp(s.head),
                        ..s
                    };
                    Target::Story(sel, v)
                }
                None => Target::Current,
            },
            Some(TextEditing::Pending { .. }) => Target::Pending,
            None => {
                let stories: Vec<(NodeId, Arc<StoryView>)> = edit
                    .selection()
                    .filter(|&n| matches!(doc.tree.kind(n), Some(NodeKind::TextStory(_))))
                    .filter(|&n| !crate::ops::on_locked_layer(doc, n))
                    .filter_map(|n| self.view(doc, n).map(|v| (n, v)))
                    .collect();
                if stories.is_empty() {
                    Target::Current
                } else {
                    Target::Stories(stories)
                }
            }
        }
    }

    /// The tab stops of the paragraph the caret is in: its ruler
    /// attribute, else the ruler its first line carries from the file.
    fn paragraph_tabs(
        doc: &Document,
        sel: TextSelection,
        v: &StoryView,
        values: &Values,
    ) -> Vec<xarast_doc::TabStop> {
        if let Some(AttrValue::Ruler(r)) = values.one(AttrSlot::TxtRuler)
            && !r.is_empty()
        {
            return r.to_vec();
        }
        let lines = xarast_doc::paragraph_line_range(&v.st, &(sel.head.byte..sel.head.byte));
        v.st.lines
            .get(lines.start)
            .and_then(|l| match doc.tree.kind(l.node) {
                Some(NodeKind::TextLine(t)) => t.ruler.as_ref().map(|r| r.to_vec()),
                _ => None,
            })
            .unwrap_or_default()
    }

    /// The text ruler of the caret's paragraph: shown for a straight story
    /// whose x axis is the page's (a turned, sheared or mirrored story, or
    /// text on a path, has no horizontal ruler to show it on).
    fn ruler(
        &self,
        doc: &Document,
        sel: TextSelection,
        v: &StoryView,
        values: &Values,
    ) -> Option<TextRuler> {
        let m = v.node.transform;
        if m.b.abs() > 1e-9 || m.c.abs() > 1e-9 || m.a <= 0.0 || m.d <= 0.0 {
            return None;
        }
        let width = match v.node.layout {
            TextLayout::AtPoint => None,
            TextLayout::InColumn { width, .. } => Some(width),
            TextLayout::OnPath { .. } => return None,
        };
        let mp = |slot| match values.one(slot) {
            Some(
                AttrValue::LeftMargin(m) | AttrValue::RightMargin(m) | AttrValue::FirstIndent(m),
            ) => *m,
            _ => Mp::ZERO,
        };
        Some(TextRuler {
            origin: m.e,
            scale: m.a,
            width,
            left_margin: mp(AttrSlot::TxtLeftMargin),
            right_margin: mp(AttrSlot::TxtRightMargin),
            first_indent: mp(AttrSlot::TxtFirstIndent),
            tabs: Self::paragraph_tabs(doc, sel, v, values),
            tab_kind: self.tab_kind,
        })
    }

    /// An infobar edit with a caret or a selection in a story.
    fn edit_story(
        &mut self,
        field: InfobarField,
        value: InfobarValue,
        sel: TextSelection,
        v: &StoryView,
        families: &[Arc<str>],
        cx: &mut ToolCtx<'_>,
    ) {
        let range = sel.range();
        let mut shown = text_infobar::story_values(&v.st, &range);
        let slot = match (field, value) {
            (InfobarField::TextFeature(_), _) => Some(AttrSlot::TxtFeatures),
            _ => text_infobar::value_for(field, value, &shown, families).and_then(|a| a.slot()),
        };
        let paragraph = slot.is_none_or(xarast_doc::is_paragraph_slot);
        if sel.is_caret() && !paragraph {
            // A character attribute at a caret: the style of what is typed
            // next, nothing in the document yet.
            for p in &self.pending {
                shown.force(p);
            }
            if let Some(a) = plain_edit(field, value, &shown, families) {
                set_slot(&mut self.pending, a);
                cx.requests.overlay_changed = true;
            }
            return;
        }
        let tabs = Self::paragraph_tabs(cx.doc, sel, v, &shown);
        let edits = story_edits(
            field,
            value,
            &v.st,
            &range,
            &shown,
            families,
            &tabs,
            self.tab_kind,
        );
        if !edits.is_empty() {
            self.edited = Some(sel.story);
            cx.commands.emit(EditCommand::SetTextAttr {
                story: sel.story,
                edits,
                burst: None,
            });
        }
    }
}

/// The caret map of a story laid out with `fonts`, as the text tool sees
/// it: for tests and for callers that want caret geometry without a tool.
/// `None` when `story` is not a text story.
#[must_use]
pub fn caret_map(doc: &Document, story: NodeId, fonts: &FontService) -> Option<CaretMap> {
    StoryView::build(doc, story, fonts, None).map(|v| v.map)
}

/// Every story on a visible, unlocked, non-guide layer, in paint order.
fn editable_stories(doc: &Document) -> impl Iterator<Item = NodeId> + '_ {
    let tree = &doc.tree;
    tree.preorder(tree.root())
        .filter(move |&id| is_editable(tree, id))
}

/// Whether `id` is a story on a visible, unlocked, non-guide layer.
fn is_editable(tree: &xarast_doc::Tree, id: NodeId) -> bool {
    matches!(tree.kind(id), Some(NodeKind::TextStory(_)))
        && tree.ancestors(id).any(|a| {
            matches!(tree.kind(a), Some(NodeKind::Layer(l)) if l.visible && !l.locked && !l.guide)
        })
}

impl TextTool {
    /// The caret, the selection and a column being dragged out.
    fn caret_overlay(&self, view: ToolView<'_>, out: &mut Vec<OverlayShape>) {
        if let Some(Drag::Column { from, to, .. }) = self.drag {
            out.push(OverlayShape::Rect {
                rect: DocRect::new(from, to),
                dashed: true,
            });
            return;
        }
        match self.editing.map(|e| e.state) {
            Some(TextEditing::Pending { at, column }) => {
                let size = self.pending_size(view.edit);
                let (ascent, descent) = (size.mul_ratio(4, 5), size.mul_ratio(1, 5));
                let from = DocPoint::new(at.x, at.y.saturating_sub(descent));
                let to = DocPoint::new(at.x, at.y.saturating_add(ascent));
                if let Some(w) = column {
                    out.push(OverlayShape::Rect {
                        rect: DocRect::new(from, DocPoint::new(at.x.saturating_add(w), to.y)),
                        dashed: true,
                    });
                }
                out.push(OverlayShape::Caret {
                    from,
                    to,
                    primary: true,
                    moved: self.moved,
                });
            }
            Some(TextEditing::Story(_)) if self.composing_caret(view.doc).is_some() => {
                self.composition_overlay(view.doc, out);
            }
            Some(TextEditing::Story(sel)) => {
                let Some(v) = self.view(view.doc, sel.story) else {
                    return;
                };
                let map = &v.map;
                let (anchor, head) = (map.snap(sel.anchor), map.snap(sel.head));
                // In story space: on a path, bent with the fitted text.
                for q in map.selection_quads(anchor.byte, head.byte) {
                    out.push(OverlayShape::Highlight {
                        corners: q.map(|p| v.to_doc(p)),
                    });
                }
                let (primary, secondary) = map.caret_segments(head);
                out.push(OverlayShape::Caret {
                    from: v.to_doc(primary.bottom),
                    to: v.to_doc(primary.top),
                    primary: true,
                    moved: self.moved,
                });
                if let Some(s) = secondary {
                    // The secondary caret: half height, from the baseline side.
                    out.push(OverlayShape::Caret {
                        from: v.to_doc(s.bottom),
                        to: v.to_doc(s.bottom.midpoint(s.top)),
                        primary: false,
                        moved: self.moved,
                    });
                }
            }
            None => {}
        }
    }
}

impl Tool for TextTool {
    fn id(&self) -> ToolId {
        ToolId::Text
    }

    fn on_activate(&mut self, cx: &mut ToolCtx<'_>) {
        // One story selected: edit it, the caret at its end.
        if cx.edit.selection_len() == 1
            && let Some(story) = cx.edit.selection().next()
            && matches!(cx.doc.tree.kind(story), Some(NodeKind::TextStory(_)))
            && let Some(v) = self.view(cx.doc, story)
        {
            let end = v
                .map
                .move_caret(Caret::at(0), CaretMotion::StoryEnd, true, None);
            self.select_story(story, end, end, cx);
        }
    }

    fn on_deactivate(&mut self, cx: &mut ToolCtx<'_>) {
        self.drag = None;
        self.set(None, None, cx);
    }

    fn on_gesture(&mut self, ev: &GestureEvent, cx: &mut ToolCtx<'_>) {
        match ev {
            GestureEvent::Click { at, count, .. } => self.click(*at, *count, cx),
            GestureEvent::DragStart { from, .. } => {
                if let Some(d) = self.indent_at(cx, *from) {
                    self.drag = Some(d);
                    cx.requests.overlay_changed = true;
                    return;
                }
                let before = self.editing;
                self.drag = Some(match self.story_at(cx, *from) {
                    Some((story, _, hit)) => {
                        let anchor = match self.current(cx.doc) {
                            Some((s, _)) if cx.modifiers.adjust && s.story == story => s.anchor,
                            _ => hit,
                        };
                        self.select_story(story, anchor, hit, cx);
                        Drag::Select {
                            story,
                            anchor,
                            before,
                        }
                    }
                    None => Drag::Column {
                        from: *from,
                        to: *from,
                        before,
                    },
                });
                cx.requests.overlay_changed = true;
            }
            GestureEvent::DragUpdate { to, .. } => match &mut self.drag {
                Some(Drag::Select { story, anchor, .. }) => {
                    let (story, anchor) = (*story, *anchor);
                    if let Some(v) = self.view(cx.doc, story) {
                        let head = v.map.hit_point(v.to_story(*to)).0;
                        self.set(
                            Some(TextEditing::Story(TextSelection {
                                story,
                                anchor,
                                head,
                            })),
                            None,
                            cx,
                        );
                    }
                }
                Some(Drag::Column { to: t, .. }) => {
                    *t = *to;
                    cx.requests.overlay_changed = true;
                }
                Some(Drag::Indent {
                    story,
                    left_end,
                    left,
                    right,
                }) => {
                    let (story, left_end, l, r) = (*story, *left_end, *left, *right);
                    let Some(v) = self.view(cx.doc, story) else {
                        return;
                    };
                    let Some(fit) = v.map.path_fit() else {
                        return;
                    };
                    let length = fit.path().length();
                    let along = fit.path().project(v.to_story(*to));
                    // Never past the other handle.
                    let (l, r) = if left_end {
                        let most = (length - r.to_f64()).max(0.0);
                        (Mp::from_f64_round(along.min(most)), r)
                    } else {
                        let most = (length - l.to_f64()).max(0.0);
                        (l, Mp::from_f64_round((length - along).clamp(0.0, most)))
                    };
                    if let Some(Drag::Indent { left, right, .. }) = &mut self.drag {
                        (*left, *right) = (l, r);
                    }
                    cx.requests.overlay_changed = true;
                }
                None => {}
            },
            GestureEvent::DragEnd { to, .. } => match self.drag.take() {
                Some(Drag::Column { from, .. }) => {
                    let width = (to.x.to_f64() - from.x.to_f64()).abs();
                    let size = Self::size_in(&[], cx.edit);
                    let left = from.x.min(to.x);
                    let top = from.y.max(to.y);
                    if width < MIN_COLUMN_PX * cx.device_px() {
                        self.pending(from, None, cx);
                    } else {
                        // The first line's top sits at the top of the drag.
                        let ascent = size.mul_ratio(4, 5);
                        let at = DocPoint::new(left, top.saturating_sub(ascent));
                        self.pending(at, Some(Mp::from_f64_round(width)), cx);
                    }
                }
                Some(Drag::Indent {
                    story, left, right, ..
                }) => {
                    let unchanged = matches!(
                        cx.doc.tree.kind(story),
                        Some(NodeKind::TextStory(t)) if matches!(t.layout,
                            TextLayout::OnPath { left_indent, right_indent, .. }
                                if left_indent == left && right_indent == right)
                    );
                    if !unchanged {
                        cx.commands.emit(EditCommand::TextPath(
                            crate::text_path_edit::TextPathEdit::Indents { story, left, right },
                        ));
                    }
                    cx.requests.overlay_changed = true;
                }
                Some(Drag::Select { .. }) | None => {
                    cx.requests.overlay_changed = true;
                }
            },
            GestureEvent::Cancel => {
                match self.drag.take() {
                    Some(Drag::Select { before, .. } | Drag::Column { before, .. }) => {
                        self.editing = before;
                        self.moved = self.moved.wrapping_add(1);
                    }
                    Some(Drag::Indent { .. }) | None => {}
                }
                cx.requests.overlay_changed = true;
            }
            GestureEvent::Hover { .. } | GestureEvent::ModifiersChanged { .. } => {}
        }
    }

    fn overlay(&self, view: ToolView<'_>, out: &mut Vec<OverlayShape>) {
        self.caret_overlay(view, out);
        // The indent handles of a story on a path, where a drag has them.
        if let Some((story, v, left, right)) = self.path_story(view.doc) {
            let (left, right) = match self.drag {
                Some(Drag::Indent {
                    story: s,
                    left,
                    right,
                    ..
                }) if s == story => (left, right),
                _ => (left, right),
            };
            if let Some(handles) = Self::indent_handles(&v, left, right) {
                for at in handles {
                    out.push(OverlayShape::Handle {
                        at,
                        shape: crate::tool::HandleShape::Radius,
                    });
                }
            }
        }
    }

    fn text_caret(&self, view: ToolView<'_>) -> Option<(DocPoint, DocPoint)> {
        match self.editing.map(|e| e.state)? {
            TextEditing::Pending { at, .. } => {
                let size = self.pending_size(view.edit);
                Some((
                    DocPoint::new(at.x, at.y.saturating_sub(size.mul_ratio(1, 5))),
                    DocPoint::new(at.x, at.y.saturating_add(size.mul_ratio(4, 5))),
                ))
            }
            TextEditing::Story(sel) => {
                let (v, caret) = match self.composing_caret(view.doc) {
                    Some((v, caret, _)) => (v, caret),
                    None => {
                        let v = self.view(view.doc, sel.story)?;
                        let head = v.map.snap(sel.head);
                        (v, head)
                    }
                };
                let (primary, _) = v.map.caret_segments(caret);
                Some((v.to_doc(primary.bottom), v.to_doc(primary.top)))
            }
        }
    }

    fn infobar(&self, view: ToolView<'_>) -> Infobar {
        let families = self.families();
        let (values, ruler) = match self.target(view.doc, view.edit) {
            Target::Story(sel, v) => {
                let mut values = text_infobar::story_values(&v.st, &sel.range());
                if sel.is_caret() {
                    for p in &self.pending {
                        values.force(p);
                    }
                }
                let ruler = self.ruler(view.doc, sel, &v, &values);
                (values, ruler)
            }
            Target::Pending => {
                let mut values =
                    text_infobar::plain_values(&view.doc.defaults, view.edit.current.values());
                for p in &self.pending {
                    values.force(p);
                }
                (values, None)
            }
            Target::Stories(stories) => {
                let mut values = Values::default();
                for (_, v) in &stories {
                    let all = text_infobar::story_values(&v.st, &(0..v.st.text.len()));
                    for s in ALL_TEXT_SLOTS {
                        for x in all.all(s) {
                            values.add(x);
                        }
                    }
                }
                (values, None)
            }
            Target::Current => (
                text_infobar::plain_values(&view.doc.defaults, view.edit.current.values()),
                None,
            ),
        };
        Infobar {
            items: text_infobar::items(&values, families, ruler),
        }
    }

    fn infobar_edit(&mut self, field: InfobarField, value: InfobarValue, cx: &mut ToolCtx<'_>) {
        if field == InfobarField::TextTabKind {
            if let InfobarValue::Choice(i) = value
                && i < text_infobar::TAB_KINDS.len()
            {
                self.tab_kind = i as u8;
            }
            return;
        }
        let families = self
            .families
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .unwrap_or_else(|| Arc::from(Vec::new()));
        // An attribute change ends a typing burst (phase 9, W9.4).
        self.burst = None;
        match self.target(cx.doc, cx.edit) {
            Target::Story(sel, v) => self.edit_story(field, value, sel, &v, &families, cx),
            Target::Pending => {
                let mut shown =
                    text_infobar::plain_values(&cx.doc.defaults, cx.edit.current.values());
                for p in &self.pending {
                    shown.force(p);
                }
                if let Some(a) = plain_edit(field, value, &shown, &families) {
                    set_slot(&mut self.pending, a);
                    cx.requests.overlay_changed = true;
                }
            }
            Target::Stories(stories) => {
                let burst = Some(next_burst());
                for (story, v) in stories {
                    let range = 0..v.st.text.len();
                    let shown = text_infobar::story_values(&v.st, &range);
                    let edits = story_edits(field, value, &v.st, &range, &shown, &families, &[], 0);
                    if !edits.is_empty() {
                        cx.commands.emit(EditCommand::SetTextAttr {
                            story,
                            edits,
                            burst,
                        });
                    }
                }
            }
            Target::Current => {
                let shown = text_infobar::plain_values(&cx.doc.defaults, cx.edit.current.values());
                if let Some(a) = plain_edit(field, value, &shown, &families) {
                    cx.requests.current.push(a);
                }
            }
        }
    }

    fn cursor(&self, _state: InteractionState) -> CursorKind {
        CursorKind::Text
    }

    fn action(&mut self, action: ToolAction, cx: &mut ToolCtx<'_>) -> bool {
        if self.editing.is_none() {
            return false;
        }
        match action {
            ToolAction::Cancel => {
                // Esc leaves the text; the story stays selected.
                self.set(None, None, cx);
                true
            }
            ToolAction::SelectAll => {
                if let Some((sel, v)) = self.current(cx.doc) {
                    let len = v.map.len();
                    self.select_story(sel.story, Caret::at(0), Caret::after(len), cx);
                }
                true
            }
            // While a caret is up these belong to the text, never to the
            // objects: Edit › Delete deletes forwards, Enter breaks the
            // paragraph. Neither joins a typing burst.
            ToolAction::Delete | ToolAction::Finish => {
                let kind = if action == ToolAction::Delete {
                    TextInputKind::Delete { word: false }
                } else {
                    TextInputKind::Insert("\n".to_owned())
                };
                self.burst = None;
                self.input(&TextInput { kind, time_ms: 0 }, cx)
            }
            _ => false,
        }
    }

    fn text_nav(&mut self, nav: TextNav, cx: &mut ToolCtx<'_>) -> bool {
        self.navigate(nav, cx)
    }

    fn text_editing(&self) -> Option<TextEditing> {
        self.editing()
    }

    fn text_input(&mut self, input: &TextInput, cx: &mut ToolCtx<'_>) -> bool {
        self.input(input, cx)
    }

    fn text_copy(&self, doc: &Document) -> Option<StyledText> {
        self.copy_selection(doc)
    }

    fn text_preedit(&mut self, preedit: Option<Preedit>, cx: &mut ToolCtx<'_>) -> bool {
        self.preedit(preedit, cx)
    }

    fn text_clipboard(&mut self, op: TextClipOp, cx: &mut ToolCtx<'_>) -> bool {
        self.clipboard(op, cx)
    }

    fn after_commands(&mut self, doc: &Document, created: Option<NodeId>) {
        if let Some(e) = self.emptying.take()
            && !doc.tree.is_reachable(e.story)
            && matches!(self.editing, Some(Editing { state: TextEditing::Story(s), .. }) if s.story == e.story)
        {
            // The edit emptied the story and removed it: the caret stays
            // where the story began, as a pending one with its style, so
            // typing on makes a new story that looks the same. Nothing to
            // join: the next key starts its own step.
            self.burst = None;
            self.adopt = None;
            self.edited = None;
            self.last_burst = None;
            self.editing = e.pending.map(|(at, column)| Editing {
                state: TextEditing::Pending { at, column },
                goal_x: None,
            });
            self.pending = if self.editing.is_some() {
                e.style
            } else {
                Vec::new()
            };
            self.moved = self.moved.wrapping_add(1);
            return;
        }
        if let Some(caret) = self.adopt.take() {
            // The story a paste at a pending caret created: the caret goes
            // on in it, after the pasted text.
            if let Some(story) =
                created.filter(|&n| matches!(doc.tree.kind(n), Some(NodeKind::TextStory(_))))
            {
                self.pending.clear();
                self.edited = Some(story);
                self.last_burst = None;
                self.editing = Some(Editing {
                    state: TextEditing::Story(TextSelection {
                        story,
                        anchor: Caret::at(caret),
                        head: Caret::at(caret),
                    }),
                    goal_x: None,
                });
                self.moved = self.moved.wrapping_add(1);
            }
            return;
        }
        let Some(b) = &mut self.burst else {
            return;
        };
        b.epoch = Some(doc.epoch.0);
        if b.story.is_none()
            && let Some(story) =
                created.filter(|&n| matches!(doc.tree.kind(n), Some(NodeKind::TextStory(_))))
        {
            // The story typing created: the caret goes on in it.
            b.story = Some(story);
            self.edited = Some(story);
            self.pending.clear();
            let caret = Caret::at(b.caret);
            self.editing = Some(Editing {
                state: TextEditing::Story(TextSelection {
                    story,
                    anchor: caret,
                    head: caret,
                }),
                goal_x: None,
            });
            self.moved = self.moved.wrapping_add(1);
        }
        // The step a removal on leaving may merge into. Should some other
        // step have come last, the history's key check keeps them apart.
        if let Some(b) = self.burst {
            self.last_burst = b.story.map(|s| (s, b.key()));
        }
    }
}
