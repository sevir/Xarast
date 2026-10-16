//! The document colour table and the derived-colour graph.

use crate::{ColourModel, ColourValue, Rgba8};
use std::collections::HashMap;
use std::sync::Arc;

slotmap::new_key_type! {
    /// A handle into a [`ColourTable`].
    ///
    /// A generational key rather than an index, so that deleting a colour
    /// that something still points at is detected rather than silently
    /// resolving to whatever took its slot.
    pub struct ColourId;
}

/// How a colour relates to its parent.
///
/// The discriminants of the `.xar` `colour_type` byte are 0 to 4 in this
/// order; the payloads are the components the format overloads for each kind.
#[derive(Clone, PartialEq, Debug, Default, serde::Serialize, serde::Deserialize)]
pub enum ColourKind {
    /// An independent colour.
    #[default]
    Normal,
    /// A named spot ink with its own separation plate.
    Spot,
    /// A tint of the parent: `factor` is how much of the parent shows,
    /// where 1.0 is the parent itself and 0.0 is white.
    Tint {
        /// The tint factor, `0.0..=1.0`, from the file's component 1.
        factor: f32,
    },
    /// Linked to the parent: components marked inherit come from the parent
    /// and the rest override it.
    Linked,
    /// A shade of the parent, positioned by two coordinates in the parent's
    /// shade space.
    Shade {
        /// Shade coordinate X, from the file's component 1.
        x: f32,
        /// Shade coordinate Y, from the file's component 2.
        y: f32,
    },
}

impl ColourKind {
    /// Reads the `.xar` `colour_type` byte, taking the overloaded components
    /// from `comps`.
    ///
    /// Unknown types become [`ColourKind::Normal`], which renders the
    /// colour's own components and so degrades to something visible rather
    /// than to nothing.
    #[must_use]
    pub fn from_byte(v: u8, comps: [Option<f32>; 4]) -> ColourKind {
        match v {
            1 => ColourKind::Spot,
            2 => ColourKind::Tint {
                factor: comps[0].unwrap_or(1.0),
            },
            3 => ColourKind::Linked,
            4 => ColourKind::Shade {
                x: comps[0].unwrap_or(0.0),
                y: comps[1].unwrap_or(0.0),
            },
            _ => ColourKind::Normal,
        }
    }

    /// Reads the `.xar` `colour_type` byte from the **raw** `FIXED24`
    /// components, which is what a reader should use.
    ///
    /// A shade's two coordinates are signed, in `[-1, 1]`, and the format
    /// writes them unclamped (`research/02 §5.10.1`), so they must not go
    /// through [`Fixed24::to_f32`](crate::Fixed24::to_f32), which clamps to
    /// `0..=1` and would turn every darkening shade into "no change".
    #[must_use]
    pub fn from_raw(v: u8, raw: [crate::Fixed24; 4]) -> ColourKind {
        let signed = |f: crate::Fixed24| {
            if f.is_inherit() {
                0.0
            } else {
                let v = f.to_f32_raw();
                if v.is_nan() { 0.0 } else { v.clamp(-1.0, 1.0) }
            }
        };
        match v {
            4 => ColourKind::Shade {
                x: signed(raw[0]),
                y: signed(raw[1]),
            },
            _ => ColourKind::from_byte(v, raw.map(crate::Fixed24::to_f32)),
        }
    }

    /// Whether the colour is derived from its parent (tint, shade or link),
    /// which is when the parent takes part in resolution at all.
    #[inline]
    #[must_use]
    pub fn is_derived(&self) -> bool {
        matches!(
            self,
            ColourKind::Tint { .. } | ColourKind::Linked | ColourKind::Shade { .. }
        )
    }
}

/// One entry in the document's colour table.
#[derive(Clone, PartialEq, Debug, serde::Serialize, serde::Deserialize)]
pub struct ColourDef {
    /// The colour's name, if it has one. `Arc<str>` because names are shared
    /// by the palette UI, the colour line and the separation list, and are
    /// never mutated.
    pub name: Option<Arc<str>>,
    /// The model the components are in.
    pub model: ColourModel,
    /// How it relates to its parent.
    pub kind: ColourKind,
    /// The parent, for tints, shades and links.
    pub parent: Option<ColourId>,
    /// The four components, where `None` means "inherit from the parent" —
    /// the `FIXED24` `-8.0` sentinel. Making inheritance a `None` rather than
    /// a magic float is what turns forgetting to check it into a compile
    /// error.
    pub components: [Option<f32>; 4],
    /// The 8-bit approximation the file carries.
    ///
    /// Not a nicety: Xara writes it precisely so that simple readers can
    /// paint without implementing the full model, and it is the correct
    /// fallback whenever resolution fails.
    pub cached_rgb: Rgba8,
    /// Position in the document's colour list, or 0 when not on the colour
    /// line.
    pub entry_index: u32,
}

impl Default for ColourDef {
    fn default() -> ColourDef {
        ColourDef {
            name: None,
            model: ColourModel::Rgbt,
            kind: ColourKind::Normal,
            parent: None,
            components: [Some(0.0), Some(0.0), Some(0.0), Some(0.0)],
            cached_rgb: Rgba8::BLACK,
            entry_index: 0,
        }
    }
}

impl ColourDef {
    /// A plain, independent colour with no parent.
    #[must_use]
    pub fn normal(value: ColourValue) -> ColourDef {
        let comps = value.components();
        ColourDef {
            name: None,
            model: value.model(),
            kind: ColourKind::Normal,
            parent: None,
            components: [
                Some(comps[0]),
                Some(comps[1]),
                Some(comps[2]),
                Some(comps[3]),
            ],
            cached_rgb: value.to_rgba8_packed(),
            entry_index: 0,
        }
    }

    /// Gives the definition a name.
    #[must_use]
    pub fn named(mut self, name: &str) -> ColourDef {
        self.name = Some(Arc::from(name));
        self
    }

    /// The components with inherited slots filled from `parent`, which must
    /// already be resolved into this definition's model.
    fn merged_components(&self, parent: Option<ColourValue>) -> [f32; 4] {
        let inherited = parent.map(|p| p.to_model(self.model).components());
        let mut out = [0.0f32; 4];
        for i in 0..4 {
            out[i] = match self.components[i] {
                Some(v) => v,
                None => inherited.map_or(0.0, |c| c[i]),
            };
        }
        out
    }
}

/// Why a colour could not be resolved.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ColourError {
    /// The parent chain is too deep or loops.
    #[error("parent chain for {0:?} exceeds the depth limit or contains a cycle")]
    ParentChain(ColourId),
    /// The handle does not name a colour in this table.
    #[error("unknown colour id {0:?}")]
    Unknown(ColourId),
}

/// The document's colour table: the definitions plus a name index.
///
/// Editing (`redefine`, `rename`, `reparent`, `remove`, `refresh_from`) lives
/// in the `edit` submodule; every mutation bumps [`ColourTable::epoch`].
#[derive(Clone, Default, Debug)]
pub struct ColourTable {
    defs: slotmap::SlotMap<ColourId, ColourDef>,
    by_name: HashMap<Arc<str>, ColourId>,
    /// Bumped by every mutation.
    epoch: PaletteEpoch,
    /// Parents before children, computed on first use after a structural
    /// change.
    order: std::sync::OnceLock<Arc<[ColourId]>>,
}

/// A version number for the palette. Monotonic: every mutation of a
/// [`ColourTable`] moves it forward, so it can key any cache that depends on
/// resolved palette colours — one palette edit invalidates exactly those.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct PaletteEpoch(pub u64);

impl PaletteEpoch {
    /// The next epoch.
    #[inline]
    #[must_use]
    pub const fn next(self) -> PaletteEpoch {
        PaletteEpoch(self.0.wrapping_add(1))
    }
}

mod edit;
pub use edit::{ColourEditError, ColourIds, OnDelete};

impl ColourTable {
    /// How far [`ColourTable::resolve`] will walk a parent chain.
    ///
    /// The format guarantees a parent is written before its child, so a
    /// well-formed file needs no limit at all. This crate must also survive
    /// a corrupt one, and a depth limit is a simpler and more predictable
    /// guard than cycle detection alone: it bounds the work as well as
    /// terminating it.
    pub const MAX_PARENT_DEPTH: usize = 16;

    /// A new, empty table.
    #[must_use]
    pub fn new() -> ColourTable {
        ColourTable::default()
    }

    /// Adds a definition and returns its handle. A repeated name replaces the
    /// earlier entry in the name index but leaves the earlier definition in
    /// place, because a reference to it may already exist.
    pub fn insert(&mut self, def: ColourDef) -> ColourId {
        let name = def.name.clone();
        let id = self.defs.insert(def);
        if let Some(n) = name {
            self.by_name.insert(n, id);
        }
        self.structure_changed();
        id
    }

    /// Records a change to which entries exist or how they are linked.
    fn structure_changed(&mut self) {
        self.order = std::sync::OnceLock::new();
        self.epoch = self.epoch.next();
    }

    /// Looks up a definition.
    #[must_use]
    pub fn get(&self, id: ColourId) -> Option<&ColourDef> {
        self.defs.get(id)
    }

    /// Repoints a colour's parent.
    ///
    /// Needed because a `.xar` file can name a parent that has not been read
    /// yet in a corrupt file, and because repointing a tint at a different
    /// base colour is an ordinary editing operation. Nothing stops this
    /// creating a cycle; [`ColourTable::resolve`] is cycle-safe precisely so
    /// that it does not have to. It is for loaders; an editor uses
    /// [`ColourTable::reparent`], which refuses cycles, and a loader calls
    /// [`ColourTable::repair_cycles`] when it is done.
    pub fn set_parent(&mut self, id: ColourId, parent: Option<ColourId>) -> bool {
        match self.defs.get_mut(id) {
            Some(d) => {
                d.parent = parent;
                self.structure_changed();
                true
            }
            None => false,
        }
    }

    /// Looks up a colour by name.
    #[must_use]
    pub fn by_name(&self, name: &str) -> Option<ColourId> {
        self.by_name.get(name).copied()
    }

    /// The number of definitions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.defs.len()
    }

    /// Whether the table holds no definitions.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }

    /// Iterates every definition with its handle.
    pub fn iter(&self) -> impl Iterator<Item = (ColourId, &ColourDef)> {
        self.defs.iter()
    }

    /// Resolves a colour to a concrete value, walking the parent chain.
    ///
    /// Depth-limited and cycle-safe. When resolution fails it falls back to
    /// the definition's `cached_rgb`, which is exactly what the format
    /// intends that field for, and to black when even the handle is unknown.
    /// Failing to a plausible colour rather than to an error is right here:
    /// a corrupt palette entry should not stop a document from opening.
    #[must_use]
    pub fn resolve(&self, id: ColourId) -> ColourValue {
        match self.try_resolve(id) {
            Ok(v) => v,
            Err(_) => self.defs.get(id).map_or(ColourValue::BLACK, |d| {
                ColourValue::from_rgba8(d.cached_rgb)
            }),
        }
    }

    /// Resolves, reporting why it could not rather than falling back.
    pub fn try_resolve(&self, id: ColourId) -> Result<ColourValue, ColourError> {
        self.resolve_inner(id, 0)
    }

    /// Resolves and quantises to 8 bits.
    #[must_use]
    pub fn resolve_rgba8(&self, id: ColourId) -> Rgba8 {
        self.resolve(id).to_rgba8()
    }

    /// The recursive worker; `depth` counts parent hops.
    fn resolve_inner(&self, id: ColourId, depth: usize) -> Result<ColourValue, ColourError> {
        let def = self.defs.get(id).ok_or(ColourError::Unknown(id))?;
        if depth >= ColourTable::MAX_PARENT_DEPTH {
            return Err(ColourError::ParentChain(id));
        }
        // Only a derived colour reads its parent (`research/02 §5.10.1`): a
        // normal or spot colour with a stale parent link is its own
        // components, as in the original.
        let parent = match def.parent {
            _ if !def.kind.is_derived() => None,
            Some(p) if p == id => return Err(ColourError::ParentChain(id)),
            Some(p) => Some(self.resolve_inner(p, depth + 1)?),
            None => None,
        };

        Ok(match def.kind {
            ColourKind::Normal | ColourKind::Spot | ColourKind::Linked => {
                ColourValue::from_components(def.model, def.merged_components(parent))
            }
            // Tints and shades apply in the child's own model to the parent
            // brought into it. The original skips the conversion because
            // making a colour a tint gives it its parent's model; converting
            // is the same thing then, and still sensible if the two drifted.
            ColourKind::Tint { factor } => parent
                .unwrap_or(ColourValue::BLACK)
                .to_model(def.model)
                .tinted(factor),
            ColourKind::Shade { x, y } => parent
                .unwrap_or(ColourValue::BLACK)
                .to_model(def.model)
                .shaded(x, y),
        })
    }

    /// Reports every cycle and depth violation in the table.
    ///
    /// Used by `xar-dump`: a document that renders correctly through the
    /// `cached_rgb` fallback still has a broken palette, and a tool that
    /// inspects files should say so.
    #[must_use]
    pub fn validate(&self) -> Vec<ColourError> {
        let mut out = Vec::new();
        for (id, _) in self.defs.iter() {
            if let Err(e) = self.try_resolve(id) {
                out.push(e);
            }
        }
        out
    }
}

/// A colour as a document attribute: either a literal value or a live
/// reference into the palette.
///
/// The distinction matters for editing, not for rendering: changing a palette
/// entry must change every object that references it, and only an
/// [`Colour::Indexed`] carries that relationship.
#[derive(Clone, PartialEq, Debug, serde::Serialize, serde::Deserialize)]
pub enum Colour {
    /// A value that belongs to this attribute alone.
    Direct(ColourValue),
    /// A reference into the document palette, optionally tinted locally.
    Indexed {
        /// The palette entry.
        id: ColourId,
        /// A local tint factor applied on top, as `.xar` allows on a fill
        /// reference without defining a new palette entry.
        tint: Option<f32>,
    },
}

impl Colour {
    /// Resolves against a palette. An unknown reference resolves to the
    /// table's own fallback rather than failing, for the same reason
    /// [`ColourTable::resolve`] does.
    #[must_use]
    pub fn resolve(&self, table: &ColourTable) -> ColourValue {
        match self {
            Colour::Direct(v) => *v,
            Colour::Indexed { id, tint } => {
                let base = table.resolve(*id);
                // A local tint is a tint of the entry in the entry's own
                // model, the same rule a palette tint follows.
                match tint {
                    None => base,
                    Some(f) => base.tinted(*f),
                }
            }
        }
    }
}

impl crate::Stop for Colour {
    /// Interpolates by resolving neither side: a gradient between two palette
    /// references is not itself a palette reference, so the result is always
    /// [`Colour::Direct`]. Interpolating the *references* would mean a stop
    /// that changes identity halfway, which nothing downstream can represent.
    ///
    /// Two [`Colour::Indexed`] stops cannot be interpolated without a table,
    /// so they fall back to whichever end `t` is nearer — the same rule
    /// [`Transparency`](crate::Transparency) uses for its mode.
    fn lerp(&self, other: &Self, t: f32, effect: crate::FillEffect) -> Colour {
        let t = if t.is_nan() { 0.0 } else { t.clamp(0.0, 1.0) };
        match (self, other) {
            (Colour::Direct(a), Colour::Direct(b)) => {
                Colour::Direct(crate::interpolate(*a, *b, t, effect))
            }
            _ => {
                if t < 0.5 {
                    self.clone()
                } else {
                    other.clone()
                }
            }
        }
    }
}

/// The serialised form of a [`ColourTable`] (the operation journal of
/// `xarast-doc`): the definitions with their exact handles, the name index
/// as it stands (a repeated name points at whichever entry won, which the
/// definitions alone cannot say) and the epoch. The parents-first order is
/// derived and rebuilt on first use.
#[derive(serde::Serialize, serde::Deserialize)]
struct ColourTableRepr {
    defs: slotmap::SlotMap<ColourId, ColourDef>,
    names: Vec<(Arc<str>, ColourId)>,
    epoch: u64,
}

impl serde::Serialize for ColourTable {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut names: Vec<(Arc<str>, ColourId)> = self
            .by_name
            .iter()
            .map(|(n, id)| (Arc::clone(n), *id))
            .collect();
        names.sort_by(|a, b| a.0.cmp(&b.0));
        ColourTableRepr {
            defs: self.defs.clone(),
            names,
            epoch: self.epoch.0,
        }
        .serialize(s)
    }
}

impl<'de> serde::Deserialize<'de> for ColourTable {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<ColourTable, D::Error> {
        let r = ColourTableRepr::deserialize(d)?;
        // A name pointing at no definition is dropped rather than refused:
        // the index is a lookup aid, and a dangling entry is what a
        // hostile input would use to make a lookup fail later.
        let by_name = r
            .names
            .into_iter()
            .filter(|(_, id)| r.defs.contains_key(*id))
            .collect();
        Ok(ColourTable {
            defs: r.defs,
            by_name,
            epoch: PaletteEpoch(r.epoch),
            order: std::sync::OnceLock::new(),
        })
    }
}
