//! The colour editor's model (phase 8, W8.6).
//!
//! The editor shows one colour — the **target** — in one colour model, and
//! turns what the user does to it into document commands. It is a model in
//! the `xarast-app` sense: it holds no widget, and `xarast-ui` draws it from
//! the [`ColourEditorView`] projection and answers with
//! [`ColourEditorOp`]s ([`crate::Intent::ColourEditor`]).
//!
//! # Targets, and the two explicit actions (T8.6.5)
//!
//! | Target | What an edit changes |
//! |---|---|
//! | [`ColourTarget::Selection`] with objects selected | the start colour of each selected object's fill (the colour of a flat fill) or line, as a **direct** colour of those objects |
//! | [`ColourTarget::Selection`] with nothing selected | the current attribute new objects are given (not undoable, as the current attributes are session state) |
//! | [`ColourTarget::Entry`] | the palette entry itself: every object using it repaints |
//!
//! Editing an object whose colour is a named colour **breaks the link**
//! for that object; it never redefines the named colour behind the user's
//! back. Redefining it is a separate, explicit action — switching the
//! target to the entry ([`ColourEditorView::linked_entry`] says which one).
//! The converse, putting a named colour on the selection as a live
//! reference, is [`ColourEditorOp::ApplyEntry`].
//!
//! # Live and committed
//!
//! A drag in the 2D field or on a slider sends [`ColourEditorOp::Preview`]
//! once per frame and [`ColourEditorOp::Commit`] on release. The first
//! preview opens a bus gesture; every preview is applied to the document
//! at once (so the canvas repaints continuously) and coalesces into **one**
//! undo step. [`ColourEditorOp::Cancel`] (`Esc` mid-drag) undoes that step
//! and drops it from the redo list, so the document *and* the history are
//! what they were before the drag. A typed number is
//! [`ColourEditorOp::Set`]: preview and commit at once.
//!
//! The editor also keeps the components it last showed. Converting a
//! colour to another model and back is not always the identity — the hue
//! of a grey is undefined — so while the document still holds exactly the
//! value the editor wrote, the editor shows its own components, and a
//! saturation dragged to zero and back keeps its hue.

use std::sync::Arc;

use xarast_color::{Colour, ColourDef, ColourId, ColourKind, ColourModel, ColourValue, OnDelete};
use xarast_doc::fill::FillGeometry;
pub use xarast_doc::fill_edit::PaintSlot;
use xarast_doc::fill_edit::{
    FillChannel, FillValue, SetStopValue, StopTarget, StopValue, fill_in_force, stop_value,
};
use xarast_doc::palette::{
    CreateColour, DeleteColour, MoveColour, RedefineColour, RenameColour, ReparentColour,
};
use xarast_doc::{AttrSlot, AttrValue, Command, EditError, NodeId, Tx};

use crate::fill_tool::FillCommand;
use crate::intent::Changed;
use crate::ops::EditCommand;
use crate::session::{Session, SessionError};

/// What the colour editor edits.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ColourTarget {
    /// The selection's fill or line colour (the current attribute when
    /// nothing is selected).
    Selection(PaintSlot),
    /// A palette entry: a named colour of the document.
    Entry(ColourId),
}

impl Default for ColourTarget {
    fn default() -> ColourTarget {
        ColourTarget::Selection(PaintSlot::Fill)
    }
}

/// How a palette entry derives from another (T8.6.4).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Derivation {
    /// An independent colour.
    Normal,
    /// An independent spot ink.
    Spot,
    /// A tint: `factor` of the parent remains, the rest is white.
    Tint {
        /// The parent.
        parent: ColourId,
        /// `0..=1`, 1 = the parent itself.
        factor: f32,
    },
    /// A shade: signed moves of the parent's saturation (`x`) and value
    /// (`y`) in HSV, each in `-1..=1`, 0 = no change.
    Shade {
        /// The parent.
        parent: ColourId,
        /// Saturation move.
        x: f32,
        /// Value move.
        y: f32,
    },
    /// A link: the components marked `inherit` follow the parent, the rest
    /// override it, all in `model`. An HSV link inheriting the hue is the
    /// "same hue, own saturation and value" colour; inheriting saturation
    /// and value instead gives a hue shift that follows the parent.
    Linked {
        /// The parent.
        parent: ColourId,
        /// The model the components are compared in.
        model: ColourModel,
        /// Which components follow the parent.
        inherit: [bool; 4],
    },
}

impl Derivation {
    /// The derivation a palette entry has now.
    #[must_use]
    pub fn of(def: &ColourDef) -> Derivation {
        match (&def.kind, def.parent) {
            (ColourKind::Tint { factor }, Some(parent)) => Derivation::Tint {
                parent,
                factor: *factor,
            },
            (ColourKind::Shade { x, y }, Some(parent)) => Derivation::Shade {
                parent,
                x: *x,
                y: *y,
            },
            (ColourKind::Linked, Some(parent)) => Derivation::Linked {
                parent,
                model: def.model,
                inherit: def.components.map(|c| c.is_none()),
            },
            (ColourKind::Spot, _) => Derivation::Spot,
            _ => Derivation::Normal,
        }
    }

    /// The parent, for the derived kinds.
    #[must_use]
    pub const fn parent(&self) -> Option<ColourId> {
        match self {
            Derivation::Normal | Derivation::Spot => None,
            Derivation::Tint { parent, .. }
            | Derivation::Shade { parent, .. }
            | Derivation::Linked { parent, .. } => Some(*parent),
        }
    }

    /// The same kind of derivation from another parent.
    #[must_use]
    pub const fn with_parent(self, parent: ColourId) -> Derivation {
        match self {
            Derivation::Normal | Derivation::Spot => self,
            Derivation::Tint { factor, .. } => Derivation::Tint { parent, factor },
            Derivation::Shade { x, y, .. } => Derivation::Shade { parent, x, y },
            Derivation::Linked { model, inherit, .. } => Derivation::Linked {
                parent,
                model,
                inherit,
            },
        }
    }

    /// What the kind is called in the editor.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Derivation::Normal => "Normal",
            Derivation::Spot => "Spot",
            Derivation::Tint { .. } => "Tint",
            Derivation::Shade { .. } => "Shade",
            Derivation::Linked { .. } => "Linked",
        }
    }

    fn kind(&self) -> ColourKind {
        match *self {
            Derivation::Normal => ColourKind::Normal,
            Derivation::Spot => ColourKind::Spot,
            Derivation::Tint { factor, .. } => ColourKind::Tint { factor },
            Derivation::Shade { x, y, .. } => ColourKind::Shade { x, y },
            Derivation::Linked { .. } => ColourKind::Linked,
        }
    }
}

/// A palette edit, as the app's [`EditCommand::Palette`] carries it: the
/// `xarast-doc` palette commands, in a form that is `Clone` and
/// comparable.
#[derive(Clone, PartialEq, Debug)]
pub enum PaletteCommand {
    /// Adds a named colour.
    Create {
        /// The definition.
        def: ColourDef,
    },
    /// Changes an entry's components and model.
    Redefine {
        /// The entry.
        id: ColourId,
        /// Its components, `None` = inherit (links only).
        components: [Option<f32>; 4],
        /// Their model.
        model: ColourModel,
    },
    /// Renames an entry.
    Rename {
        /// The entry.
        id: ColourId,
        /// The new name.
        name: Arc<str>,
    },
    /// Changes what an entry derives from and how.
    Derive {
        /// The entry.
        id: ColourId,
        /// The new derivation.
        derivation: Derivation,
    },
    /// Deletes an entry.
    Delete {
        /// The entry.
        id: ColourId,
        /// What happens to its uses.
        policy: OnDelete,
    },
    /// Moves a named entry along the colour line (phase 8, W8.7).
    Move {
        /// The entry.
        id: ColourId,
        /// Its new position in [`xarast_color::ColourTable::listed`].
        to: usize,
    },
}

impl PaletteCommand {
    /// The undo label.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            PaletteCommand::Create { .. } => "Create Colour",
            PaletteCommand::Redefine { .. } => "Edit Colour",
            PaletteCommand::Rename { .. } => "Rename Colour",
            PaletteCommand::Derive { .. } => "Link Colour",
            PaletteCommand::Delete { .. } => "Delete Colour",
            PaletteCommand::Move { .. } => "Move Colour",
        }
    }

    /// The entry it edits (none for a creation).
    #[must_use]
    pub const fn id(&self) -> Option<ColourId> {
        match self {
            PaletteCommand::Create { .. } => None,
            PaletteCommand::Redefine { id, .. }
            | PaletteCommand::Rename { id, .. }
            | PaletteCommand::Derive { id, .. }
            | PaletteCommand::Delete { id, .. }
            | PaletteCommand::Move { id, .. } => Some(*id),
        }
    }

    /// Whether it merges with `prev` inside one gesture: a drag redefining
    /// one entry, or dragging one entry's tint amount.
    #[must_use]
    pub fn coalesces_with(&self, prev: &PaletteCommand) -> bool {
        match (self, prev) {
            (PaletteCommand::Redefine { id: a, .. }, PaletteCommand::Redefine { id: b, .. })
            | (PaletteCommand::Derive { id: a, .. }, PaletteCommand::Derive { id: b, .. }) => {
                a == b
            }
            _ => false,
        }
    }

    pub(crate) fn run(&self, tx: &mut Tx<'_>) -> Result<(), EditError> {
        match self {
            PaletteCommand::Create { def } => CreateColour::new(def.clone()).run(tx),
            PaletteCommand::Redefine {
                id,
                components,
                model,
            } => RedefineColour {
                id: *id,
                components: *components,
                model: *model,
            }
            .run(tx),
            PaletteCommand::Rename { id, name } => RenameColour {
                id: *id,
                name: name.clone(),
            }
            .run(tx),
            PaletteCommand::Derive { id, derivation } => {
                ReparentColour {
                    id: *id,
                    kind: derivation.kind(),
                    parent: derivation.parent(),
                }
                .run(tx)?;
                if let Derivation::Linked { model, inherit, .. } = *derivation {
                    let table = &tx.doc().resources.colours;
                    let now = table.resolve(*id).to_model(model).components();
                    let components: [Option<f32>; 4] =
                        std::array::from_fn(|i| (!inherit[i]).then_some(now[i]));
                    RedefineColour {
                        id: *id,
                        components,
                        model,
                    }
                    .run(tx)?;
                }
                Ok(())
            }
            PaletteCommand::Delete { id, policy } => DeleteColour {
                id: *id,
                policy: *policy,
            }
            .run(tx),
            PaletteCommand::Move { id, to } => MoveColour { id: *id, to: *to }.run(tx),
        }
    }
}

/// A change the editor asks for.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ColourChange {
    /// New components, in the view's model.
    Components([f32; 4]),
    /// A new derivation for the target entry.
    Derivation(Derivation),
}

/// What the colour editor panel asks for.
#[derive(Clone, PartialEq, Debug)]
pub enum ColourEditorOp {
    /// Edit something else. Commits a drag in flight.
    SetTarget(ColourTarget),
    /// Show the colour in another model. For a palette entry this is an
    /// edit — the entry's model is what it is defined in — except for a
    /// tint or shade, whose model is its parent's.
    SetModel(ColourModel),
    /// A live change: the first one of a drag opens it, and every one is
    /// applied at once into the drag's single undo step.
    Preview(ColourChange),
    /// A change made in one go (a typed number, a click): a preview and a
    /// commit.
    Set(ColourChange),
    /// Ends the drag in flight, keeping what it did.
    Commit,
    /// Abandons the drag in flight: the document and the history go back
    /// to what they were before it.
    Cancel,
    /// Renames the target entry.
    Rename(String),
    /// Makes a new named colour holding the colour shown, and edits it.
    NewNamed(String),
    /// Puts a named colour on the selection's fill or line as a live
    /// reference (the current attribute when nothing is selected).
    ApplyEntry {
        /// The named colour.
        id: ColourId,
        /// Fill or line.
        slot: PaintSlot,
    },
}

/// A named colour, as the editor lists it.
#[derive(Clone, PartialEq, Debug)]
pub struct NamedColour {
    /// The entry.
    pub id: ColourId,
    /// Its name.
    pub name: String,
    /// What it resolves to.
    pub value: ColourValue,
    /// Whether the target entry could derive from it: not itself and not
    /// one of its own descendants. Always true for a selection target.
    pub can_parent: bool,
}

/// What the editor shows this frame.
#[derive(Clone, PartialEq, Debug)]
pub struct ColourEditorView {
    /// What is being edited.
    pub target: ColourTarget,
    /// Says so in words: "Fill of 2 objects", "Line for new objects",
    /// "Colour ‘Sky’".
    pub title: String,
    /// The model the components are in.
    pub model: ColourModel,
    /// Whether the model can be changed (not for a tint or a shade).
    pub model_editable: bool,
    /// The components, in `model`.
    pub components: [f32; 4],
    /// Which components can be edited: none for a tint or a shade (edit
    /// the derivation), the overriding ones for a link.
    pub editable: [bool; 4],
    /// The colour, resolved.
    pub value: ColourValue,
    /// The value before the drag in flight, for a before/after preview.
    pub original: Option<ColourValue>,
    /// A drag is in flight.
    pub dragging: bool,
    /// For a selection target: the named colour its colour refers to, so
    /// that "Redefine" can be offered as its own action.
    pub linked_entry: Option<NamedColour>,
    /// For an entry target: its name and derivation.
    pub entry: Option<(String, Derivation)>,
    /// Every named colour of the document, in palette order.
    pub named: Vec<NamedColour>,
    /// How many objects the selection target covers (0 = the current
    /// attribute).
    pub objects: usize,
}

/// The editor's own state: held by the session, never undone.
#[derive(Debug, Clone, Default)]
pub struct ColourEditorModel {
    target: ColourTarget,
    model: ColourModel,
    shown: Option<Shown>,
    drag: Option<LiveDrag>,
}

/// The components last shown and the value they stand for.
#[derive(Debug, Clone, Copy)]
struct Shown {
    target: ColourTarget,
    model: ColourModel,
    components: [f32; 4],
    value: ColourValue,
}

/// A drag in flight.
#[derive(Debug, Clone)]
struct LiveDrag {
    gesture: u64,
    serial_before: u64,
    applied: usize,
    original: ColourValue,
    current_before: crate::edit::CurrentAttributes,
}

impl ColourEditorModel {
    /// What is being edited.
    #[must_use]
    pub const fn target(&self) -> ColourTarget {
        self.target
    }

    /// The model a selection target is shown in.
    #[must_use]
    pub const fn model(&self) -> ColourModel {
        self.model
    }

    /// Whether a drag is in flight.
    #[must_use]
    pub const fn dragging(&self) -> bool {
        self.drag.is_some()
    }
}

/// What the target currently holds: the colour as the document stores it,
/// resolved, and in which model the editor must show it.
struct Current {
    colour: Colour,
    value: ColourValue,
    model: ColourModel,
    editable: [bool; 4],
    model_editable: bool,
}

fn selected(session: &Session) -> Vec<NodeId> {
    session.edit.selection().collect()
}

fn slot_attr(slot: PaintSlot) -> AttrSlot {
    FillChannel::Colour.attr_slot(slot)
}

/// The colour the selection target holds: the first selected object's
/// fill start (a flat fill's colour) or line colour, else the current
/// attribute, else the document default.
fn selection_colour(session: &Session, slot: PaintSlot) -> Colour {
    if slot == PaintSlot::Fill
        && let Some((nodes, target)) = crate::colour_bar::selected_stop(session)
        && let Some(&n) = nodes.first()
        && let FillValue::Colour(g) = fill_in_force(&session.doc, n, slot, FillChannel::Colour)
        && let Some(c) = stop_value(&g, target)
    {
        return c;
    }
    let paint = match selected(session).first() {
        Some(&n) => match fill_in_force(&session.doc, n, slot, FillChannel::Colour) {
            FillValue::Colour(g) => Some(g),
            FillValue::Transparency(_) => None,
        },
        None => session
            .edit
            .current
            .values()
            .iter()
            .find(|v| v.slot() == Some(slot_attr(slot)))
            .or_else(|| Some(session.doc.defaults.get(slot_attr(slot)).as_ref()))
            .and_then(|v| match v {
                AttrValue::Fill(g) | AttrValue::StrokeColour(g) => Some(g.clone()),
                _ => None,
            }),
    };
    paint
        .and_then(|g| stop_value(&g, StopTarget::From))
        .unwrap_or(Colour::Direct(ColourValue::BLACK))
}

fn current(session: &Session, ed: &ColourEditorModel) -> Option<Current> {
    let table = &session.doc.resources.colours;
    match ed.target {
        ColourTarget::Selection(slot) => {
            let colour = selection_colour(session, slot);
            let value = colour.resolve(table);
            Some(Current {
                colour,
                value,
                model: display_model(ed.model),
                editable: [true; 4],
                model_editable: true,
            })
        }
        ColourTarget::Entry(id) => {
            let def = table.get(id)?;
            let value = table.resolve(id);
            let (editable, model_editable) = match def.kind {
                ColourKind::Tint { .. } | ColourKind::Shade { .. } => ([false; 4], false),
                ColourKind::Linked => (def.components.map(|c| c.is_some()), true),
                ColourKind::Normal | ColourKind::Spot => ([true; 4], true),
            };
            Some(Current {
                colour: Colour::Indexed { id, tint: None },
                value,
                model: def.model,
                editable,
                model_editable,
            })
        }
    }
}

/// The models the editor offers for a direct colour. The others (CIE, the
/// web palette, the internal indexed model) show as RGB.
fn display_model(m: ColourModel) -> ColourModel {
    match m {
        ColourModel::Rgbt | ColourModel::Hsvt | ColourModel::Greyt | ColourModel::Cmyk => m,
        _ => ColourModel::Rgbt,
    }
}

fn named(session: &Session, target: ColourTarget) -> Vec<NamedColour> {
    let table = &session.doc.resources.colours;
    table
        .iter()
        .filter_map(|(id, def)| {
            let name = def.name.as_deref()?;
            let can_parent = match target {
                ColourTarget::Entry(t) => id != t && !table.derives_from(id, t),
                ColourTarget::Selection(_) => true,
            };
            Some(NamedColour {
                id,
                name: name.to_owned(),
                value: table.resolve(id),
                can_parent,
            })
        })
        .collect()
}

fn entry_name(session: &Session, id: ColourId) -> String {
    session
        .doc
        .resources
        .colours
        .get(id)
        .and_then(|d| d.name.as_deref())
        .unwrap_or("Unnamed colour")
        .to_owned()
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "object" } else { "objects" }
}

/// The projection the interface draws.
#[must_use]
pub fn view(session: &Session) -> Option<ColourEditorView> {
    let ed = &session.colour_editor;
    let cur = current(session, ed)?;
    let components = match ed.shown {
        Some(s) if s.target == ed.target && s.model == cur.model && s.value == cur.value => {
            s.components
        }
        _ => cur.value.to_model(cur.model).components(),
    };
    let objects = match ed.target {
        ColourTarget::Selection(_) => session.edit.selection_len(),
        ColourTarget::Entry(_) => 0,
    };
    let title = match ed.target {
        ColourTarget::Selection(slot) => {
            let what = match slot {
                PaintSlot::Fill => match crate::colour_bar::selected_stop(session) {
                    Some((_, t)) => format!("Fill {}", crate::colour_bar::stop_label(t)),
                    None => "Fill".to_owned(),
                },
                PaintSlot::Stroke => "Line".to_owned(),
            };
            if objects == 0 {
                format!("{what} for new objects")
            } else {
                format!("{what} of {objects} {}", plural(objects))
            }
        }
        ColourTarget::Entry(id) => format!("Colour \u{2018}{}\u{2019}", entry_name(session, id)),
    };
    let named = named(session, ed.target);
    let linked_entry = match (ed.target, &cur.colour) {
        (ColourTarget::Selection(_), Colour::Indexed { id, tint: None }) => {
            named.iter().find(|n| n.id == *id).cloned()
        }
        _ => None,
    };
    let entry = match ed.target {
        ColourTarget::Entry(id) => session
            .doc
            .resources
            .colours
            .get(id)
            .map(|d| (entry_name(session, id), Derivation::of(d))),
        ColourTarget::Selection(_) => None,
    };
    Some(ColourEditorView {
        target: ed.target,
        title,
        model: cur.model,
        model_editable: cur.model_editable,
        components,
        editable: cur.editable,
        value: cur.value,
        original: ed.drag.as_ref().map(|d| d.original),
        dragging: ed.drag.is_some(),
        linked_entry,
        entry,
        named,
        objects,
    })
}

/// The same components with the non-editable ones taken from `now`: a
/// link's inherited components and a derived colour's are not the
/// editor's to change.
fn masked(new: [f32; 4], now: [f32; 4], editable: [bool; 4]) -> [f32; 4] {
    std::array::from_fn(|i| if editable[i] { new[i] } else { now[i] })
}

/// Sets the target's colour. Returns whether the document changed.
fn write(
    session: &mut Session,
    ed: &mut ColourEditorModel,
    change: ColourChange,
) -> Result<Changed, SessionError> {
    let Some(cur) = current(session, ed) else {
        return Ok(Changed::empty());
    };
    match (ed.target, change) {
        (ColourTarget::Selection(slot), ColourChange::Components(c)) => {
            let value = ColourValue::from_components(cur.model, c);
            ed.shown = Some(Shown {
                target: ed.target,
                model: cur.model,
                components: c,
                value,
            });
            set_selection_colour(session, slot, Colour::Direct(value))
        }
        (ColourTarget::Entry(id), ColourChange::Components(c)) => {
            if !cur.editable.iter().any(|e| *e) {
                return Ok(Changed::empty());
            }
            let Some(def) = session.doc.resources.colours.get(id) else {
                return Ok(Changed::empty());
            };
            let now = cur.value.to_model(cur.model).components();
            let c = masked(c, now, cur.editable);
            let components: [Option<f32>; 4] = std::array::from_fn(|i| {
                if def.kind == ColourKind::Linked && def.components[i].is_none() {
                    None
                } else {
                    Some(c[i].clamp(0.0, 1.0))
                }
            });
            let model = def.model;
            let changed = apply(
                session,
                EditCommand::Palette(PaletteCommand::Redefine {
                    id,
                    components,
                    model,
                }),
            )?;
            let value = session.doc.resources.colours.resolve(id);
            ed.shown = Some(Shown {
                target: ed.target,
                model,
                components: c,
                value,
            });
            Ok(changed)
        }
        (ColourTarget::Entry(id), ColourChange::Derivation(derivation)) => {
            ed.shown = None;
            apply(
                session,
                EditCommand::Palette(PaletteCommand::Derive { id, derivation }),
            )
        }
        (ColourTarget::Selection(_), ColourChange::Derivation(_)) => Ok(Changed::empty()),
    }
}

fn apply(session: &mut Session, cmd: EditCommand) -> Result<Changed, SessionError> {
    Ok(if session.apply_edit(cmd)?.is_some() {
        Changed::DOCUMENT | Changed::UI
    } else {
        Changed::empty()
    })
}

/// Puts `colour` on the selection's fill start or line, or makes it the
/// current attribute when nothing is selected.
fn set_selection_colour(
    session: &mut Session,
    slot: PaintSlot,
    colour: Colour,
) -> Result<Changed, SessionError> {
    let nodes = selected(session);
    if nodes.is_empty() {
        let paint = FillGeometry::Flat { value: colour };
        let value = match slot {
            PaintSlot::Fill => AttrValue::Fill(paint),
            PaintSlot::Stroke => AttrValue::StrokeColour(paint),
        };
        return Ok(if session.edit.current.set(value) {
            Changed::UI
        } else {
            Changed::empty()
        });
    }
    // The fill tool's selected handle, when there is one, is what the
    // colour goes to (XARA-T-0247); otherwise the start colour.
    let (nodes, target) = match crate::colour_bar::selected_stop(session) {
        Some(stop) if slot == PaintSlot::Fill => stop,
        _ => (nodes, StopTarget::From),
    };
    let edits = nodes
        .into_iter()
        .map(|node| {
            FillCommand::SetStopValue(SetStopValue {
                node,
                slot,
                channel: FillChannel::Colour,
                target,
                value: StopValue::Colour(colour.clone()),
            })
        })
        .collect();
    apply(session, EditCommand::Fill { edits })
}

fn start_drag(session: &mut Session, ed: &mut ColourEditorModel) {
    if ed.drag.is_some() {
        return;
    }
    let original = current(session, ed).map_or(ColourValue::BLACK, |c| c.value);
    let serial_before = session.bus.history().state_serial();
    let current_before = session.edit.current.clone();
    // The drag's first command would drop a redo branch that Esc must
    // bring back: set it aside until the drag ends.
    session.bus.hold_redo(&mut session.doc);
    session.flush_journal();
    let gesture = session.begin_gesture();
    ed.drag = Some(LiveDrag {
        gesture,
        serial_before,
        applied: 0,
        original,
        current_before,
    });
}

fn commit(session: &mut Session, ed: &mut ColourEditorModel) -> Changed {
    match ed.drag.take() {
        Some(d) => {
            session.end_gesture(d.gesture);
            end_hold(session, d.serial_before);
            Changed::UI
        }
        None => Changed::empty(),
    }
}

/// Settles the redo branch `start_drag` set aside: back when the history is
/// where the drag found it (nothing applied, or all of it undone),
/// forgotten otherwise, as the drag's commit would have done.
fn end_hold(session: &mut Session, serial_before: u64) {
    if session.bus.history().state_serial() == serial_before {
        session.bus.restore_held_redo(&mut session.doc);
    } else {
        session.bus.release_held_redo(&mut session.doc);
    }
    session.flush_journal();
}

fn cancel(session: &mut Session, ed: &mut ColourEditorModel) -> Changed {
    let Some(d) = ed.drag.take() else {
        return Changed::empty();
    };
    session.end_gesture(d.gesture);
    let mut changed = Changed::UI;
    if session.edit.current != d.current_before {
        session.edit.current = d.current_before;
    }
    if d.applied > 0 {
        let mut undone = 0;
        while session.bus.history().state_serial() != d.serial_before
            && undone < d.applied
            && session.undo().is_some()
        {
            undone += 1;
        }
        if undone > 0 {
            session.bus.discard_redo(&mut session.doc);
            session.flush_journal();
            changed |= Changed::DOCUMENT;
        }
    }
    end_hold(session, d.serial_before);
    ed.shown = None;
    changed
}

/// Ends a drag in flight before something else touches the history (an
/// undo, a redo), keeping what it did.
pub(crate) fn settle(session: &mut Session) -> Changed {
    let mut ed = std::mem::take(&mut session.colour_editor);
    let changed = commit(session, &mut ed);
    session.colour_editor = ed;
    changed
}

/// A named colour is about to go: the editor stops editing it (and ends a
/// drag in flight) so that it never shows a colour that is not there.
pub(crate) fn forget_entry(session: &mut Session, id: ColourId) -> Changed {
    let mut changed = settle(session);
    if session.colour_editor.target == ColourTarget::Entry(id) {
        session.colour_editor.target = ColourTarget::default();
        session.colour_editor.shown = None;
        changed |= Changed::UI;
    }
    changed
}

/// Runs one editor operation.
///
/// # Errors
///
/// Whatever the command it dispatches returns (a locked layer, a palette
/// cycle, a name in use); the document is left as it was and a drag in
/// flight stays open.
pub(crate) fn run(session: &mut Session, op: ColourEditorOp) -> Result<Changed, SessionError> {
    let mut ed = std::mem::take(&mut session.colour_editor);
    let out = run_with(session, &mut ed, op);
    session.colour_editor = ed;
    out
}

fn run_with(
    session: &mut Session,
    ed: &mut ColourEditorModel,
    op: ColourEditorOp,
) -> Result<Changed, SessionError> {
    match op {
        ColourEditorOp::SetTarget(target) => {
            let mut changed = commit(session, ed);
            if ed.target != target {
                ed.target = target;
                ed.shown = None;
                changed |= Changed::UI;
            }
            Ok(changed)
        }
        ColourEditorOp::SetModel(model) => {
            let mut changed = commit(session, ed);
            match ed.target {
                ColourTarget::Selection(_) => {
                    if ed.model != model {
                        ed.model = display_model(model);
                        ed.shown = None;
                        changed |= Changed::UI;
                    }
                }
                ColourTarget::Entry(id) => {
                    let Some(def) = session.doc.resources.colours.get(id) else {
                        return Ok(changed);
                    };
                    if def.model == model {
                        return Ok(changed);
                    }
                    let cmd = match Derivation::of(def) {
                        Derivation::Normal | Derivation::Spot => {
                            let value = session.doc.resources.colours.resolve(id);
                            PaletteCommand::Redefine {
                                id,
                                components: value.to_model(model).components().map(Some),
                                model,
                            }
                        }
                        Derivation::Linked {
                            parent, inherit, ..
                        } => PaletteCommand::Derive {
                            id,
                            derivation: Derivation::Linked {
                                parent,
                                model,
                                inherit,
                            },
                        },
                        Derivation::Tint { .. } | Derivation::Shade { .. } => return Ok(changed),
                    };
                    ed.shown = None;
                    changed |= apply(session, EditCommand::Palette(cmd))?;
                }
            }
            Ok(changed)
        }
        ColourEditorOp::Preview(change) => {
            start_drag(session, ed);
            let changed = write(session, ed, change)?;
            if changed.contains(Changed::DOCUMENT)
                && let Some(d) = ed.drag.as_mut()
            {
                d.applied += 1;
            }
            Ok(changed | Changed::UI)
        }
        ColourEditorOp::Set(change) => {
            if ed.drag.is_some() {
                let changed = run_with(session, ed, ColourEditorOp::Preview(change))?;
                return Ok(changed | commit(session, ed));
            }
            write(session, ed, change)
        }
        ColourEditorOp::Commit => Ok(commit(session, ed)),
        ColourEditorOp::Cancel => Ok(cancel(session, ed)),
        ColourEditorOp::Rename(name) => {
            let mut changed = commit(session, ed);
            if let ColourTarget::Entry(id) = ed.target {
                let name = name.trim();
                if !name.is_empty() && entry_name(session, id) != name {
                    changed |= apply(
                        session,
                        EditCommand::Palette(PaletteCommand::Rename {
                            id,
                            name: Arc::from(name),
                        }),
                    )?;
                }
            }
            Ok(changed)
        }
        ColourEditorOp::NewNamed(name) => {
            let mut changed = commit(session, ed);
            let name = name.trim();
            let Some(cur) = current(session, ed) else {
                return Ok(changed);
            };
            if name.is_empty() {
                return Ok(changed);
            }
            let value = cur.value.to_model(cur.model);
            changed |= apply(
                session,
                EditCommand::Palette(PaletteCommand::Create {
                    def: ColourDef::normal(value).named(name),
                }),
            )?;
            if let Some(id) = session.doc.resources.colours.by_name(name) {
                ed.target = ColourTarget::Entry(id);
                ed.shown = None;
            }
            Ok(changed | Changed::UI)
        }
        ColourEditorOp::ApplyEntry { id, slot } => {
            let changed = commit(session, ed);
            if session.doc.resources.colours.get(id).is_none() {
                return Ok(changed);
            }
            Ok(changed | set_selection_colour(session, slot, Colour::Indexed { id, tint: None })?)
        }
    }
}

/// The unique name `"{stem} n"` a new colour gets by default.
#[must_use]
pub fn fresh_name(session: &Session, stem: &str) -> String {
    let table = &session.doc.resources.colours;
    (1..)
        .map(|n| format!("{stem} {n}"))
        .find(|n| table.by_name(n).is_none())
        .unwrap_or_else(|| stem.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_link_lists_what_it_inherits() {
        let mut t = xarast_color::ColourTable::new();
        let p = t.insert(ColourDef::normal(ColourValue::rgb(1.0, 0.0, 0.0)).named("P"));
        let c = t.insert(ColourDef::normal(ColourValue::rgb(0.0, 1.0, 0.0)).named("C"));
        t.reparent(c, ColourKind::Linked, Some(p)).unwrap();
        t.redefine(c, [None, Some(0.5), None, Some(0.0)], ColourModel::Rgbt)
            .unwrap();
        assert_eq!(
            Derivation::of(t.get(c).unwrap()),
            Derivation::Linked {
                parent: p,
                model: ColourModel::Rgbt,
                inherit: [true, false, true, false],
            }
        );
        assert_eq!(Derivation::of(t.get(p).unwrap()), Derivation::Normal);
    }

    #[test]
    fn only_edits_of_one_entry_coalesce() {
        let mut t = xarast_color::ColourTable::new();
        let a = t.insert(ColourDef::normal(ColourValue::BLACK));
        let b = t.insert(ColourDef::normal(ColourValue::WHITE));
        let r = |id| PaletteCommand::Redefine {
            id,
            components: [Some(0.0); 4],
            model: ColourModel::Rgbt,
        };
        assert!(r(a).coalesces_with(&r(a)));
        assert!(!r(a).coalesces_with(&r(b)));
        let d = PaletteCommand::Derive {
            id: a,
            derivation: Derivation::Tint {
                parent: b,
                factor: 0.5,
            },
        };
        assert!(d.coalesces_with(&d));
        assert!(!d.coalesces_with(&r(a)));
        assert_eq!(d.label(), "Link Colour");
    }

    #[test]
    fn non_editable_components_are_kept() {
        assert_eq!(
            masked([1.0, 1.0, 1.0, 1.0], [0.0; 4], [true, false, true, false]),
            [1.0, 0.0, 1.0, 0.0]
        );
    }
}
