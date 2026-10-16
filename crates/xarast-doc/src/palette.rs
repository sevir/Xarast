//! The document palette seen from the document: undoable palette commands,
//! the reverse index of colour uses, and the scene-build resolver for
//! [`Colour::Indexed`] (phase 8, W8.1).
//!
//! # Palette edits swap the whole table
//!
//! Every palette command computes the new [`ColourTable`] and applies it as
//! one [`Action::SetPalette`], whose inverse is the table as it was. A
//! palette is a few hundred entries, so the clone is microseconds, and it
//! buys exact undo for free: a removed entry comes back **under its old
//! [`ColourId`]**, which a per-entry "re-insert" could not promise with a
//! generational slot map. The epoch still moves forward on undo, so no
//! palette-keyed cache can serve a stale entry.
//!
//! # A palette edit repaints without walking the tree
//!
//! [`Colour::Indexed`] is resolved at scene build through
//! [`PaletteResolver`], whose memo is keyed on the palette epoch. Which
//! objects need repainting is [`ColourUses::users`] of the ids an edit
//! reports as changed; the index is rebuilt from scratch on load and is
//! never serialised.

use std::collections::HashMap;
use std::sync::Arc;

use smallvec::SmallVec;
use xarast_color::{
    Colour, ColourContext, ColourDef, ColourId, ColourIds, ColourKind, ColourModel, ColourTable,
    OnDelete, PaletteEpoch, Rgba8,
};

use crate::Document;
use crate::attr::AttrValue;
use crate::digest::CanonicalHasher;
use crate::fill::FillGeometry;
use crate::history::{Action, Command, EditError, Tx};
use crate::kind::NodeKind;
use crate::tree::NodeId;

impl Document {
    /// The palette epoch: part of every cache key that depends on resolved
    /// palette colours.
    #[must_use]
    pub fn palette_epoch(&self) -> PaletteEpoch {
        self.resources.colours.epoch()
    }
}

/// Folds the colour table into the canonical digest.
///
/// Entries in slot order with their ids, because references in the tree are
/// hashed by id too; the epoch and the cached resolution order are not
/// content and are left out, which is what lets an undo return the digest.
pub(crate) fn digest_palette(table: &ColourTable, h: &mut CanonicalHasher) {
    h.str("palette");
    h.len(table.len());
    for (id, def) in table.iter() {
        h.u64(slotmap::Key::data(&id).as_ffi());
        h.opt(&def.name.as_deref());
        h.u8(def.model as u8);
        match def.kind {
            ColourKind::Normal => h.u8(0),
            ColourKind::Spot => h.u8(1),
            ColourKind::Tint { factor } => {
                h.u8(2);
                h.f32(factor);
            }
            ColourKind::Linked => h.u8(3),
            ColourKind::Shade { x, y } => {
                h.u8(4);
                h.f32(x);
                h.f32(y);
            }
        }
        match def.parent {
            Some(p) => h.u64(slotmap::Key::data(&p).as_ffi()),
            None => h.u64(0),
        }
        for c in def.components {
            match c {
                Some(v) => {
                    h.u8(1);
                    h.f32(v);
                }
                None => h.u8(0),
            }
        }
        h.bytes(&[
            def.cached_rgb.r,
            def.cached_rgb.g,
            def.cached_rgb.b,
            def.cached_rgb.a,
        ]);
        h.u32(def.entry_index);
    }
}

// ─────────────────────────── visiting colours ───────────────────────────

/// Calls `f` on every colour a fill holds: endpoints, ramp stops, corner
/// colours, contone pair.
pub fn for_each_colour(g: &FillGeometry<Colour>, mut f: impl FnMut(&Colour)) {
    match g {
        FillGeometry::Flat { value } => f(value),
        FillGeometry::Linear { from, to, ramp, .. }
        | FillGeometry::Radial { from, to, ramp, .. }
        | FillGeometry::Conical { from, to, ramp, .. }
        | FillGeometry::Diamond { from, to, ramp, .. } => {
            f(from);
            for s in ramp.stops() {
                f(&s.value);
            }
            f(to);
        }
        FillGeometry::ThreeColour { c0, c1, c2, .. } => {
            f(c0);
            f(c1);
            f(c2);
        }
        FillGeometry::FourColour { c0, c1, c2, c3, .. } => {
            f(c0);
            f(c1);
            f(c2);
            f(c3);
        }
        FillGeometry::Bitmap { contone, .. } => {
            if let Some((a, b)) = contone {
                f(a);
                f(b);
            }
        }
        FillGeometry::Fractal { from, to, .. } | FillGeometry::Noise { from, to, .. } => {
            f(from);
            f(to);
        }
    }
}

/// Rewrites every colour a fill holds through `f`, returning the new fill.
#[must_use]
pub fn map_colours(
    g: &FillGeometry<Colour>,
    f: &impl Fn(&Colour) -> Colour,
) -> FillGeometry<Colour> {
    let mut g = g.clone();
    match &mut g {
        FillGeometry::Flat { value } => *value = f(value),
        FillGeometry::Linear { from, to, ramp, .. }
        | FillGeometry::Radial { from, to, ramp, .. }
        | FillGeometry::Conical { from, to, ramp, .. }
        | FillGeometry::Diamond { from, to, ramp, .. } => {
            *from = f(from);
            *to = f(to);
            *ramp = crate::fill_edit::rebuild_ramp(ramp, |s| crate::fill::RampStop {
                pos: s.pos,
                value: f(&s.value),
            });
        }
        FillGeometry::ThreeColour { c0, c1, c2, .. } => {
            *c0 = f(c0);
            *c1 = f(c1);
            *c2 = f(c2);
        }
        FillGeometry::FourColour { c0, c1, c2, c3, .. } => {
            *c0 = f(c0);
            *c1 = f(c1);
            *c2 = f(c2);
            *c3 = f(c3);
        }
        FillGeometry::Bitmap { contone, .. } => {
            if let Some((a, b)) = contone {
                *a = f(a);
                *b = f(b);
            }
        }
        FillGeometry::Fractal { from, to, .. } | FillGeometry::Noise { from, to, .. } => {
            *from = f(from);
            *to = f(to);
        }
    }
    g
}

/// The palette ids a node refers to: through a colour attribute, a layer's
/// guide colour or a guideline's colour.
pub fn palette_refs(kind: &NodeKind, out: &mut SmallVec<[ColourId; 4]>) {
    let mut push = |c: &Colour| {
        if let Colour::Indexed { id, .. } = c
            && !out.contains(id)
        {
            out.push(*id);
        }
    };
    match kind {
        NodeKind::Attr(a) => match &a.value {
            AttrValue::Fill(p) | AttrValue::StrokeColour(p) => for_each_colour(p, &mut push),
            _ => {}
        },
        NodeKind::Layer(l) => {
            if let Some(c) = l.guide_colour
                && !out.contains(&c)
            {
                out.push(c);
            }
        }
        NodeKind::Guideline(g) => {
            if let Some(c) = g.colour
                && !out.contains(&c)
            {
                out.push(c);
            }
        }
        _ => {}
    }
}

// ─────────────────────────────── ColourUses ─────────────────────────────

/// Reverse index `ColourId -> [NodeId]`: which nodes must repaint when a
/// palette entry's resolved value changes.
///
/// For a colour attribute the user recorded is the attribute's **owner**
/// (its parent), because an attribute's scope is its owner's subtree, so
/// the owner's bounds cover everything it paints. Denormalised state:
/// rebuilt from scratch on load, never serialised. The commands that change
/// attributes do not maintain it yet — rebuild after a batch of edits, or
/// see `docs/memory/colour.md` for the incremental plan.
#[derive(Clone, Debug, Default)]
pub struct ColourUses {
    users: HashMap<ColourId, SmallVec<[NodeId; 4]>>,
}

impl ColourUses {
    /// Builds the index for a document: every reachable node.
    #[must_use]
    pub fn build(doc: &Document) -> ColourUses {
        let mut u = ColourUses::default();
        u.rebuild(doc);
        u
    }

    /// Rebuilds the index from scratch.
    pub fn rebuild(&mut self, doc: &Document) {
        self.users.clear();
        let mut refs: SmallVec<[ColourId; 4]> = SmallVec::new();
        for n in doc.tree.preorder(doc.tree.root()) {
            let Some(kind) = doc.tree.kind(n) else {
                continue;
            };
            refs.clear();
            palette_refs(kind, &mut refs);
            if refs.is_empty() {
                continue;
            }
            let user = match kind {
                NodeKind::Attr(_) => doc.tree.links(n).parent.unwrap_or(n),
                _ => n,
            };
            for c in &refs {
                let v = self.users.entry(*c).or_default();
                if !v.contains(&user) {
                    v.push(user);
                }
            }
        }
    }

    /// The nodes that use `id`.
    #[must_use]
    pub fn users(&self, id: ColourId) -> &[NodeId] {
        self.users.get(&id).map_or(&[], |v| v.as_slice())
    }

    /// The nodes that use any of `ids`, each once: what a palette edit
    /// reporting `ids` as changed must repaint.
    #[must_use]
    pub fn users_of(&self, ids: &[ColourId]) -> Vec<NodeId> {
        let mut out: Vec<NodeId> = Vec::new();
        for id in ids {
            for n in self.users(*id) {
                if !out.contains(n) {
                    out.push(*n);
                }
            }
        }
        out
    }

    /// Whether anything uses `id`.
    #[must_use]
    pub fn is_used(&self, id: ColourId) -> bool {
        !self.users(id).is_empty()
    }
}

// ─────────────────────────────── resolver ───────────────────────────────

/// Resolves attribute colours to 8-bit sRGB at scene build, memoised per
/// palette entry and invalidated by the palette epoch.
///
/// This is the doc-side half of T8.1.4: the scene walker holds one of these
/// and calls [`PaletteResolver::resolve`] for every [`Colour`] it paints, and
/// folds [`PaletteResolver::epoch`] into its cache keys.
#[derive(Clone, Debug, Default)]
pub struct PaletteResolver {
    ctx: ColourContext,
    epoch: Option<PaletteEpoch>,
    memo: HashMap<(ColourId, u32), Rgba8>,
}

impl PaletteResolver {
    /// A resolver with an empty memo.
    #[must_use]
    pub fn new() -> PaletteResolver {
        PaletteResolver::default()
    }

    /// Resolves a colour against `table`. A direct colour is converted; a
    /// palette reference is looked up once per epoch.
    pub fn resolve(&mut self, c: &Colour, table: &ColourTable) -> Rgba8 {
        match c {
            Colour::Direct(v) => self.ctx.srgb_of(*v),
            // "No colour" paints nothing: transparent.
            Colour::None => Rgba8 {
                r: 0,
                g: 0,
                b: 0,
                a: 0,
            },
            Colour::Indexed { id, tint } => {
                if self.epoch != Some(table.epoch()) {
                    self.memo.clear();
                    self.epoch = Some(table.epoch());
                }
                let key = (*id, tint.map_or(u32::MAX, f32::to_bits));
                *self
                    .memo
                    .entry(key)
                    .or_insert_with(|| self.ctx.resolve(c, table))
            }
        }
    }

    /// The epoch the memo belongs to, once anything has been resolved.
    #[must_use]
    pub fn epoch(&self) -> Option<PaletteEpoch> {
        self.epoch
    }
}

// ─────────────────────────────── commands ───────────────────────────────

/// Swaps in `table` as one undoable action.
fn commit_table(tx: &mut Tx<'_>, table: ColourTable) -> Result<(), EditError> {
    tx.act(Action::SetPalette {
        new: Arc::new(table),
    })
}

/// Adds a named colour. The new id is recorded in `created` once the
/// command has run (a command cannot return a value through the bus).
#[derive(Debug)]
pub struct CreateColour {
    /// The definition. Its cached value is recomputed.
    pub def: ColourDef,
    /// Filled in by `run`.
    pub created: std::cell::Cell<Option<ColourId>>,
}

impl CreateColour {
    /// A command that adds `def`.
    #[must_use]
    pub fn new(def: ColourDef) -> CreateColour {
        CreateColour {
            def,
            created: std::cell::Cell::new(None),
        }
    }
}

impl Command for CreateColour {
    fn label(&self) -> &'static str {
        "Create Colour"
    }

    fn run(&self, tx: &mut Tx<'_>) -> Result<(), EditError> {
        let mut t = tx.doc().resources.colours.clone();
        if let Some(n) = &self.def.name
            && t.by_name(n).is_some()
        {
            return Err(xarast_color::ColourEditError::NameInUse.into());
        }
        let mut def = self.def.clone();
        // A new named colour joins the end of the colour line.
        if def.name.is_some() && def.entry_index == 0 {
            def.entry_index = t.next_entry_index();
        }
        let (kind, parent) = (def.kind.clone(), def.parent);
        def.kind = ColourKind::Normal;
        def.parent = None;
        let id = t.insert(def);
        if kind.is_derived() || parent.is_some() {
            t.reparent(id, kind, parent)?;
        } else {
            t.refresh_from(id);
        }
        commit_table(tx, t)?;
        self.created.set(Some(id));
        Ok(())
    }
}

/// Moves a named colour along the colour line, to position `to` of
/// [`ColourTable::listed`]. The order is the entries' `entry_index`, which
/// both file formats keep. Moving an entry to where it already is leaves
/// the palette untouched (a caller that wants no empty undo step checks
/// [`ColourTable::listed`] first).
#[derive(Clone, Debug)]
pub struct MoveColour {
    /// The entry; it must be named.
    pub id: ColourId,
    /// Its new position in the listed order.
    pub to: usize,
}

impl Command for MoveColour {
    fn label(&self) -> &'static str {
        "Move Colour"
    }

    fn run(&self, tx: &mut Tx<'_>) -> Result<(), EditError> {
        let mut t = tx.doc().resources.colours.clone();
        if t.move_listed(self.id, self.to)? {
            commit_table(tx, t)
        } else {
            Ok(())
        }
    }
}

/// Changes a palette entry's components and model; every use repaints.
#[derive(Clone, Debug)]
pub struct RedefineColour {
    /// The entry.
    pub id: ColourId,
    /// Its new components, `None` = inherit (linked colours only).
    pub components: [Option<f32>; 4],
    /// The model they are in.
    pub model: ColourModel,
}

impl Command for RedefineColour {
    fn label(&self) -> &'static str {
        "Edit Colour"
    }

    fn run(&self, tx: &mut Tx<'_>) -> Result<(), EditError> {
        let mut t = tx.doc().resources.colours.clone();
        t.redefine(self.id, self.components, self.model)?;
        commit_table(tx, t)
    }
}

/// Renames a palette entry.
#[derive(Clone, Debug)]
pub struct RenameColour {
    /// The entry.
    pub id: ColourId,
    /// Its new name; must not be taken.
    pub name: Arc<str>,
}

impl Command for RenameColour {
    fn label(&self) -> &'static str {
        "Rename Colour"
    }

    fn run(&self, tx: &mut Tx<'_>) -> Result<(), EditError> {
        let mut t = tx.doc().resources.colours.clone();
        t.rename(self.id, self.name.clone())?;
        commit_table(tx, t)
    }
}

/// Changes what a palette entry derives from (normal, spot, tint, shade,
/// link) — refused if it would close a cycle.
#[derive(Clone, Debug)]
pub struct ReparentColour {
    /// The entry.
    pub id: ColourId,
    /// Its new kind.
    pub kind: ColourKind,
    /// Its new parent: `Some` for derived kinds, `None` otherwise.
    pub parent: Option<ColourId>,
}

impl Command for ReparentColour {
    fn label(&self) -> &'static str {
        "Link Colour"
    }

    fn run(&self, tx: &mut Tx<'_>) -> Result<(), EditError> {
        let mut t = tx.doc().resources.colours.clone();
        t.reparent(self.id, self.kind.clone(), self.parent)?;
        commit_table(tx, t)
    }
}

/// Deletes a palette entry.
///
/// Under [`OnDelete::Reject`] it fails if any object, layer, guideline or
/// derived colour still uses the entry. Under [`OnDelete::Detach`] — the
/// original's forced delete — every use keeps its appearance: attribute
/// references become [`Colour::Direct`] holding what they resolved to
/// (local tint included), guide colours fall back to the default, derived
/// entries become normal colours. All in one undo step.
#[derive(Clone, Debug)]
pub struct DeleteColour {
    /// The entry.
    pub id: ColourId,
    /// What to do with its uses.
    pub policy: OnDelete,
}

impl Command for DeleteColour {
    fn label(&self) -> &'static str {
        "Delete Colour"
    }

    fn run(&self, tx: &mut Tx<'_>) -> Result<(), EditError> {
        let id = self.id;
        if tx.doc().resources.colours.get(id).is_none() {
            return Err(xarast_color::ColourEditError::NotFound.into());
        }
        // Every node in the arena, reachable or retained by the history, so
        // that an undone deletion of an object cannot bring back a dangling
        // reference.
        let users: Vec<NodeId> = {
            let doc = tx.doc();
            let mut refs: SmallVec<[ColourId; 4]> = SmallVec::new();
            doc.tree
                .iter()
                .filter(|(_, d)| {
                    refs.clear();
                    palette_refs(&d.kind, &mut refs);
                    refs.contains(&id)
                })
                .map(|(n, _)| n)
                .collect()
        };
        if self.policy == OnDelete::Reject && !users.is_empty() {
            return Err(xarast_color::ColourEditError::StillReferenced.into());
        }
        let mut table = tx.doc().resources.colours.clone();
        table.remove(id, self.policy)?;

        let old = tx.doc().resources.colours.clone();
        let detach = |c: &Colour| match c {
            Colour::Indexed { id: r, .. } if *r == id => Colour::Direct(c.resolve(&old)),
            other => other.clone(),
        };
        for n in users {
            let Some(kind) = tx.doc().tree.kind(n).cloned() else {
                continue;
            };
            match kind {
                NodeKind::Attr(a) => {
                    let value = match &a.value {
                        AttrValue::Fill(p) => AttrValue::Fill(map_colours(p, &detach)),
                        AttrValue::StrokeColour(p) => {
                            AttrValue::StrokeColour(map_colours(p, &detach))
                        }
                        _ => continue,
                    };
                    tx.act(Action::SetAttr {
                        node: n,
                        new: Arc::new(value),
                    })?;
                }
                NodeKind::Layer(mut l) => {
                    l.guide_colour = None;
                    tx.act(Action::SetKind {
                        node: n,
                        new: Box::new(NodeKind::Layer(l)),
                    })?;
                }
                NodeKind::Guideline(mut g) => {
                    g.colour = None;
                    tx.act(Action::SetKind {
                        node: n,
                        new: Box::new(NodeKind::Guideline(g)),
                    })?;
                }
                _ => {}
            }
        }
        commit_table(tx, table)
    }
}

/// The ids a palette edit changed, for a caller that wants to repaint
/// without re-running the edit: compares cached values of two tables.
#[must_use]
pub fn changed_between(before: &ColourTable, after: &ColourTable) -> ColourIds {
    let mut out = ColourIds::new();
    for (id, d) in after.iter() {
        match before.get(id) {
            Some(b) if b.cached_rgb == d.cached_rgb => {}
            _ => out.push(id),
        }
    }
    for (id, _) in before.iter() {
        if after.get(id).is_none() {
            out.push(id);
        }
    }
    out
}
