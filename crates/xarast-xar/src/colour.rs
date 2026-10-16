//! Colour definitions and the record-number registry that resolves them.
//!
//! `TAG_DEFINECOMPLEXCOLOUR` (51) is only 0.41 % of corpus records but
//! appears in **all 59 files**, which is why it is in the minimum viable set
//! while far more frequent tags are not.
//!
//! Two things a first implementation gets wrong:
//!
//! * the component value `0xF800_0000` reads as `-8.0` and means **inherit
//!   this component from the parent colour**, not a colour
//!   (`research/01 §11` item 9). [`Fixed24::INHERIT`] and the
//!   `[Option<f32>; 4]` component type in `xarast-color` make forgetting it
//!   a compile error;
//! * the first three bytes are an 8-bit RGB approximation Xara has already
//!   computed, precisely so that a simple reader can paint without
//!   implementing CMYK, HSV and the tint graph. It is the right fallback
//!   whenever resolution fails, and it is *not* redundant.
//!
//! Parents are always written before their children — the original asserts
//! it — so one pass with an incrementally filled map resolves the whole
//! graph.

use std::collections::HashMap;

use crate::cur::Cur;
use crate::diag::{DiagCode, DiagSink, Diagnostic};
use crate::error::XarError;
use crate::tags::Ref;
use xarast_color::{
    BuiltinColour, Colour, ColourDef, ColourId, ColourKind, ColourModel, ColourTable, ColourValue,
    Fixed24, Rgba8,
};

/// A colour definition record, decoded but not yet resolved.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ColourRecord {
    /// The 8-bit approximation the file carries.
    pub rgb: Rgba8,
    /// The colour model byte, 0–6.
    pub model: ColourModel,
    /// The colour type byte: 0 normal, 1 spot, 2 tint, 3 linked, 4 shade.
    pub colour_type: u8,
    /// Position in the document colour list, 0 when not on the colour line.
    pub entry_index: u32,
    /// The parent colour, for tints, shades and links.
    pub parent: Ref,
    /// The four components, sentinels included.
    pub components: [Fixed24; 4],
    /// The colour's name, which may be empty.
    pub name: Option<String>,
}

impl ColourRecord {
    /// Decodes `TAG_DEFINECOMPLEXCOLOUR` (51).
    ///
    /// # Errors
    ///
    /// [`XarError::ShortRecord`] when the fixed 29-byte prefix is not there.
    /// A missing name is tolerated: the record is then simply unnamed.
    pub fn parse_complex(cur: &mut Cur<'_>) -> Result<ColourRecord, XarError> {
        let r = cur.u8()?;
        let g = cur.u8()?;
        let b = cur.u8()?;
        let model = ColourModel::from_byte(cur.u8()?);
        let colour_type = cur.u8()?;
        let entry_index = cur.u32()?;
        let parent = cur.reference()?;
        let components = [
            cur.fixed24()?,
            cur.fixed24()?,
            cur.fixed24()?,
            cur.fixed24()?,
        ];
        let name = cur.utf16_z().ok().filter(|s| !s.is_empty());
        Ok(ColourRecord {
            rgb: Rgba8::rgb(r, g, b),
            model,
            colour_type,
            entry_index,
            parent,
            components,
            name,
        })
    }

    /// Decodes `TAG_DEFINERGBCOLOUR` (50): three bytes, nothing else.
    ///
    /// Never observed in the corpus — Xara always writes complex colours or
    /// uses the negative built-ins — but cheap to support and present in old
    /// files.
    ///
    /// # Errors
    ///
    /// [`XarError::ShortRecord`] when fewer than three bytes remain.
    pub fn parse_rgb(cur: &mut Cur<'_>) -> Result<ColourRecord, XarError> {
        let r = cur.u8()?;
        let g = cur.u8()?;
        let b = cur.u8()?;
        Ok(ColourRecord {
            rgb: Rgba8::rgb(r, g, b),
            model: ColourModel::Rgbt,
            colour_type: 0,
            entry_index: 0,
            parent: Ref::None,
            components: [
                Fixed24::from_f32(f32::from(r) / 255.0),
                Fixed24::from_f32(f32::from(g) / 255.0),
                Fixed24::from_f32(f32::from(b) / 255.0),
                Fixed24::ZERO,
            ],
            name: None,
        })
    }

    /// The components as `Option<f32>`, where `None` is the inherit
    /// sentinel.
    #[must_use]
    pub fn components_f32(&self) -> [Option<f32>; 4] {
        [
            self.components[0].to_f32(),
            self.components[1].to_f32(),
            self.components[2].to_f32(),
            self.components[3].to_f32(),
        ]
    }

    /// Converts into a palette entry, given the resolved parent.
    #[must_use]
    pub fn to_colour_def(&self, parent: Option<ColourId>) -> ColourDef {
        let comps = self.components_f32();
        ColourDef {
            name: self.name.as_deref().map(std::sync::Arc::from),
            model: self.model,
            // From the raw components: a shade's coordinates are signed.
            kind: ColourKind::from_raw(self.colour_type, self.components),
            parent,
            components: comps,
            cached_rgb: self.rgb,
            entry_index: self.entry_index,
        }
    }
}

/// Maps `.xar` record numbers onto palette entries.
///
/// One pass is enough because the format guarantees a definition precedes
/// its first use, and a parent precedes its child.
#[derive(Clone, Debug, Default)]
pub struct ColourRegistry {
    table: ColourTable,
    by_record: HashMap<u32, ColourId>,
}

impl ColourRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> ColourRegistry {
        ColourRegistry::default()
    }

    /// Registers a colour definition read at `record`.
    ///
    /// A parent that has not been seen — which the format forbids, so it
    /// means the file is damaged — produces a
    /// [`DanglingReference`](crate::DiagCode::DanglingReference) diagnostic
    /// and a parentless colour, which still paints as its cached RGB.
    pub fn define(&mut self, record: u32, def: &ColourRecord, diags: &mut DiagSink) -> ColourId {
        let parent = self.parent_of(record, def, diags);
        let id = self.table.insert(def.to_colour_def(parent));
        self.by_record.insert(record, id);
        id
    }

    /// Registers a definition whose palette entry lives **somewhere else**.
    ///
    /// The mapping stage needs the palette to end up in the document's own
    /// [`ColourTable`], and the only way to put it there is
    /// [`DocumentBuilder::define_colour`](xarast_doc::DocumentBuilder::define_colour),
    /// which allocates the [`ColourId`]. So the registry keeps doing the part
    /// that is genuinely its own — resolving the parent chain from record
    /// numbers, and diagnosing a parent the file never defined — and `insert`
    /// supplies the entry.
    ///
    /// In this mode [`ColourRegistry::table`] stays empty; the ids in
    /// [`ColourRegistry::resolve`]'s results belong to whatever table
    /// `insert` wrote to.
    pub fn define_external(
        &mut self,
        record: u32,
        def: &ColourRecord,
        diags: &mut DiagSink,
        insert: impl FnOnce(xarast_color::ColourDef) -> ColourId,
    ) -> ColourId {
        let parent = self.parent_of(record, def, diags);
        let id = insert(def.to_colour_def(parent));
        self.by_record.insert(record, id);
        id
    }

    fn parent_of(&self, record: u32, def: &ColourRecord, diags: &mut DiagSink) -> Option<ColourId> {
        match def.parent {
            Ref::None | Ref::Builtin(_) => None,
            Ref::Record(n) => {
                let found = self.by_record.get(&n).copied();
                if found.is_none() {
                    diags.push(
                        Diagnostic::new(DiagCode::DanglingReference)
                            .at(record, crate::tags::TAG_DEFINECOMPLEXCOLOUR)
                            .with_detail(u64::from(n)),
                    );
                }
                found
            }
        }
    }

    /// Resolves a colour reference read from a fill or line attribute.
    ///
    /// * a negative value is one of the ten built-ins, −1 being "no
    ///   colour";
    /// * zero is "no colour";
    /// * a positive value is a record number, and a number that names no
    ///   definition is a diagnostic plus black, never a failure.
    ///
    /// "No colour" is [`Colour::None`], and so is an unknown built-in
    /// (with a diagnostic): painting nothing is safer than inventing one.
    pub fn resolve(&self, reference: Ref, diags: &mut DiagSink, at: (u32, u32)) -> Colour {
        match reference {
            Ref::None => Colour::None,
            Ref::Builtin(v) => match BuiltinColour::from_ref(v) {
                Some(BuiltinColour::None) => Colour::None,
                Some(b) => b.value().map_or(Colour::None, Colour::Direct),
                None => {
                    diags.push(
                        Diagnostic::new(DiagCode::UnknownEnumValue)
                            .at(at.0, at.1)
                            .with_detail(v.unsigned_abs().into()),
                    );
                    Colour::None
                }
            },
            Ref::Record(n) => match self.by_record.get(&n) {
                Some(&id) => Colour::Indexed { id, tint: None },
                None => {
                    diags.push(
                        Diagnostic::new(DiagCode::DanglingReference)
                            .at(at.0, at.1)
                            .with_detail(u64::from(n)),
                    );
                    Colour::Direct(ColourValue::BLACK)
                }
            },
        }
    }

    /// The palette built so far.
    #[must_use]
    pub const fn table(&self) -> &ColourTable {
        &self.table
    }

    /// How many colours have been defined.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_record.len()
    }

    /// Whether no colour has been defined.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_record.is_empty()
    }

    /// The palette entry a record number names, if any.
    #[must_use]
    pub fn id_of(&self, record: u32) -> Option<ColourId> {
        self.by_record.get(&record).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn complex(
        rgb: [u8; 3],
        model: u8,
        kind: u8,
        entry: u32,
        parent: i32,
        comps: [f32; 4],
        name: &str,
    ) -> Vec<u8> {
        let mut v = rgb.to_vec();
        v.push(model);
        v.push(kind);
        v.extend_from_slice(&entry.to_le_bytes());
        v.extend_from_slice(&parent.to_le_bytes());
        for c in comps {
            v.extend_from_slice(&Fixed24::from_f32(c).0.to_le_bytes());
        }
        for u in name.encode_utf16() {
            v.extend_from_slice(&u.to_le_bytes());
        }
        v.extend_from_slice(&0u16.to_le_bytes());
        v
    }

    /// `testfiles/OneLine.xar` record 31 (`research/01 §9.3`).
    #[test]
    fn the_worked_black_decodes() {
        let bytes = complex([0, 0, 0], 3, 0, 24, 0, [0.0, 0.0, 0.0, 1.0], "Black");
        assert_eq!(bytes.len(), 41);
        let c = ColourRecord::parse_complex(&mut Cur::new(&bytes)).unwrap();
        assert_eq!(c.model, ColourModel::Cmyk);
        assert_eq!(c.colour_type, 0);
        assert_eq!(c.entry_index, 24);
        assert_eq!(c.parent, Ref::None);
        assert_eq!(c.name.as_deref(), Some("Black"));
        assert_eq!(c.rgb, Rgba8::rgb(0, 0, 0));
    }

    /// Record 69 of the same file: a 90 % tint of record 31.
    #[test]
    fn a_tint_resolves_against_its_parent() {
        let mut reg = ColourRegistry::new();
        let mut d = DiagSink::new();
        let black = complex([0, 0, 0], 3, 0, 24, 0, [0.0, 0.0, 0.0, 1.0], "Black");
        let parent_rec = ColourRecord::parse_complex(&mut Cur::new(&black)).unwrap();
        reg.define(31, &parent_rec, &mut d);
        let tint = complex(
            [25, 25, 25],
            3,
            2,
            25,
            31,
            [0.9, 0.0, 0.0, 0.0],
            "90% Black",
        );
        let tint_rec = ColourRecord::parse_complex(&mut Cur::new(&tint)).unwrap();
        let id = reg.define(69, &tint_rec, &mut d);
        assert_eq!(d.total(), 0);
        let def = reg.table().get(id).unwrap();
        assert!(matches!(def.kind, ColourKind::Tint { .. }));
        assert!(def.parent.is_some());
    }

    #[test]
    fn the_inherit_sentinel_survives_as_none() {
        let mut bytes = complex([1, 2, 3], 2, 3, 0, 0, [0.0; 4], "");
        // Overwrite component 2 with the sentinel.
        let at = 13 + 4;
        bytes[at..at + 4].copy_from_slice(&Fixed24::INHERIT.0.to_le_bytes());
        let c = ColourRecord::parse_complex(&mut Cur::new(&bytes)).unwrap();
        assert_eq!(c.components_f32()[1], None);
        assert_eq!(c.name, None);
    }

    #[test]
    fn a_nameless_colour_is_thirty_one_bytes() {
        let bytes = complex([1, 2, 3], 2, 0, 0, 0, [0.0; 4], "");
        assert_eq!(bytes.len(), 31);
        assert!(ColourRecord::parse_complex(&mut Cur::new(&bytes)).is_ok());
    }

    #[test]
    fn a_colour_record_missing_its_name_terminator_still_parses() {
        let mut bytes = complex([1, 2, 3], 2, 0, 0, 0, [0.0; 4], "x");
        bytes.truncate(bytes.len() - 2);
        let c = ColourRecord::parse_complex(&mut Cur::new(&bytes)).unwrap();
        assert_eq!(c.name, None);
    }

    #[test]
    fn every_truncation_of_a_colour_record_is_handled() {
        let bytes = complex([1, 2, 3], 4, 2, 7, 3, [0.5; 4], "name");
        for n in 0..bytes.len() {
            let _ = ColourRecord::parse_complex(&mut Cur::new(&bytes[..n]));
        }
    }

    #[test]
    fn builtin_references_resolve_without_a_table() {
        let reg = ColourRegistry::new();
        let mut d = DiagSink::new();
        assert!(reg.resolve(Ref::parse(-1), &mut d, (1, 150)).is_none());
        assert!(!reg.resolve(Ref::parse(-2), &mut d, (1, 150)).is_none());
        assert!(reg.resolve(Ref::parse(0), &mut d, (1, 150)).is_none());
        assert_eq!(d.total(), 0);
    }

    #[test]
    fn a_dangling_reference_is_black_and_a_diagnostic() {
        let reg = ColourRegistry::new();
        let mut d = DiagSink::new();
        let c = reg.resolve(Ref::parse(999), &mut d, (1, 150));
        assert_eq!(c, Colour::Direct(ColourValue::BLACK));
        assert_eq!(d.count(crate::Severity::Warning), 1);
    }

    #[test]
    fn an_unknown_negative_reference_warns_rather_than_painting_something() {
        let reg = ColourRegistry::new();
        let mut d = DiagSink::new();
        assert!(reg.resolve(Ref::parse(-99), &mut d, (1, 150)).is_none());
        assert_eq!(d.count(crate::Severity::Warning), 1);
    }

    #[test]
    fn a_parent_cycle_cannot_be_built_from_a_single_pass() {
        // The registry only ever links to colours already defined, so the
        // cycle the fuzz target hunts for is structurally impossible here.
        let mut reg = ColourRegistry::new();
        let mut d = DiagSink::new();
        let self_ref = complex([0, 0, 0], 2, 3, 0, 5, [0.0; 4], "");
        let rec = ColourRecord::parse_complex(&mut Cur::new(&self_ref)).unwrap();
        let id = reg.define(5, &rec, &mut d);
        assert!(reg.table().get(id).unwrap().parent.is_none());
        assert_eq!(d.count(crate::Severity::Warning), 1);
        // Resolution terminates.
        let _ = reg.table().resolve(id);
    }
}
