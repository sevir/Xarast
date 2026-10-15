//! Tag numbers, the tag classification table and the unknown-tag policy.
//!
//! # Why the policy is three-way and why it is here from the start
//!
//! A `.xar` file carries its own forward-compatibility rules. Two records
//! near the top of the stream list tags by number: `TAG_ATOMICTAGS` (10) and
//! `TAG_ESSENTIALTAGS` (11). A reader that meets a tag it has no handler for
//! consults them:
//!
//! * **essential** — the file cannot be represented without it, so stop;
//! * **atomic** — it is a composite node whose children are *derived* data,
//!   so drop the record **and its whole subtree**;
//! * **anything else** — skip the record's bytes and carry on.
//!
//! Getting the middle case wrong is not a cosmetic bug. A shadow
//! controller's children are the shadow record *and a duplicate of the
//! shadowed object*; insert those loose into the tree and the document gains
//! a phantom object that paints as if it were real (`research/01 §11`
//! item 13). Newer commercial Xara versions write tags above 4138 that this
//! reader has never seen, and this mechanism is exactly how they stay
//! readable, so it is implemented now rather than deferred.
//!
//! The atomic list arrives spread over **many** records — 739 of them in the
//! corpus, four bytes each, one tag apiece — never as one list. Read
//! `size / 4` entries from each and take the union.

use std::collections::BTreeSet;

use crate::tags_table::TAG_TABLE;

/// Move up one level: closes the scope opened by [`TAG_DOWN`].
pub const TAG_UP: u32 = 0;
/// Move down one level: subsequent records are children of the last node.
pub const TAG_DOWN: u32 = 1;
/// The logical file header. Always the first record, never compressed.
pub const TAG_FILEHEADER: u32 = 2;
/// End of file. The parser stops here and does not assume `pos == len`.
pub const TAG_ENDOFFILE: u32 = 3;
/// A list of atomic tags, `size / 4` of them.
pub const TAG_ATOMICTAGS: u32 = 10;
/// A list of essential tags, `size / 4` of them.
pub const TAG_ESSENTIALTAGS: u32 = 11;
/// Tag descriptions for user-facing warnings.
pub const TAG_TAGDESCRIPTION: u32 = 12;
/// From the next byte on, the stream is raw DEFLATE.
pub const TAG_STARTCOMPRESSION: u32 = 30;
/// End of a compressed block. Its eight payload bytes are CRC-32 and length,
/// and they are **not** inside the deflate stream.
pub const TAG_ENDCOMPRESSION: u32 = 31;
/// The document root node.
pub const TAG_DOCUMENT: u32 = 40;
/// A chapter node.
pub const TAG_CHAPTER: u32 = 41;
/// A spread node.
pub const TAG_SPREAD: u32 = 42;
/// A layer node.
pub const TAG_LAYER: u32 = 43;
/// Page size, margin, bleed and spread flags. Sets the coordinate origin.
pub const TAG_SPREADINFORMATION: u32 = 45;
/// Layer flags and name.
pub const TAG_LAYERDETAILS: u32 = 48;
/// A guide layer's flags, name and guide colour.
pub const TAG_GUIDELAYERDETAILS: u32 = 49;
/// A three-byte RGB colour definition.
pub const TAG_DEFINERGBCOLOUR: u32 = 50;
/// A full colour definition: model, type, components, parent and name.
pub const TAG_DEFINECOMPLEXCOLOUR: u32 = 51;
/// A group node.
pub const TAG_GROUP: u32 = 104;
/// A ClipView controller: a group whose children before its
/// [`TAG_CLIPVIEW`] marker clip the children after it.
pub const TAG_CLIPVIEWCONTROLLER: u32 = 4084;
/// The marker inside a ClipView controller between its clipping objects
/// and the objects they clip.
pub const TAG_CLIPVIEW: u32 = 4085;
/// A shadow controller: its attributes, one [`TAG_SHADOW`] and the object
/// that casts the shadow.
pub const TAG_SHADOWCONTROLLER: u32 = 4050;
/// The generated shadow node inside a shadow controller.
pub const TAG_SHADOW: u32 = 4051;
/// A bevel controller: its attributes, the object(s) it bevels and one
/// [`TAG_BEVELINK`].
pub const TAG_BEVEL: u32 = 4052;
/// The generated bevel node inside a bevel controller: before the objects
/// for an outer bevel, after them for an inner one.
pub const TAG_BEVELINK: u32 = 4057;
/// A contour controller: its attributes, the object(s) it contours and one
/// [`TAG_CONTOUR`].
pub const TAG_CONTOURCONTROLLER: u32 = 4066;
/// The generated contour node inside a contour controller: before the
/// objects for an outer contour, after them for an inner one.
pub const TAG_CONTOUR: u32 = 4067;
/// A blend: its attributes, the objects it blends and one [`TAG_BLENDER`]
/// between each pair; the [`TAG_BLENDPROFILES`] before it is its profiles.
pub const TAG_BLEND: u32 = 105;
/// One pair of a blend, between its two objects.
pub const TAG_BLENDER: u32 = 106;
/// A blender's extra fields, the record after its [`TAG_BLENDER`].
pub const TAG_BLENDERADDITIONAL: u32 = 4073;
/// The profiles of the next [`TAG_BLEND`].
pub const TAG_BLENDPROFILES: u32 = 4072;
/// A path a blend runs along.
pub const TAG_BLEND_PATH: u32 = 4061;
/// Whether the last blend path is filled.
pub const TAG_NODEBLENDPATH_FILLED: u32 = 4074;
/// Per-point path flags, the first child of the path record.
pub const TAG_PATH_FLAGS: u32 = 111;
/// A relative path, stroked.
pub const TAG_PATH_RELATIVE_STROKED: u32 = 115;
/// A relative path, filled and stroked. The single most common object record.
pub const TAG_PATH_RELATIVE_FILLED_STROKED: u32 = 116;
/// A generic regular shape, version 2. The only shape record Xara X writes.
pub const TAG_REGULAR_SHAPE_PHASE_2: u32 = 1901;
/// The document's current attributes: an atomic container of defaults.
pub const TAG_CURRENTATTRIBUTES: u32 = 4119;

/// What a tag does to the importer's state.
///
/// The distinction that matters is [`TagClass::Structural`]: an unhandled
/// structural tag means the tree we built is not the tree the file describes,
/// and the acceptance criteria count those specifically.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum TagClass {
    /// Builds or closes tree structure. Not understanding it corrupts the
    /// tree.
    Structural,
    /// Pushes an attribute into the current scope.
    Attribute,
    /// Registers a referenceable definition.
    Definition,
    /// An object node in the drawing.
    Object,
    /// Safe to skip: printing, view state, editor hints.
    Ignorable,
}

impl TagClass {
    /// The stable name used in reports and snapshots.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            TagClass::Structural => "structural",
            TagClass::Attribute => "attribute",
            TagClass::Definition => "definition",
            TagClass::Object => "object",
            TagClass::Ignorable => "ignorable",
        }
    }
}

/// What to do with a tag we have no handler for.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum UnknownAction {
    /// Skip the record's bytes and carry on.
    Skip,
    /// Skip the record and everything up to the `UP` that matches the `DOWN`
    /// following it.
    StripSubtree,
    /// Stop: the file cannot be represented.
    Abort,
}

/// The symbolic name of a tag, for reports.
#[must_use]
pub fn name_of(tag: u32) -> Option<&'static str> {
    lookup(tag).map(|e| e.1)
}

/// The static classification of a tag.
#[must_use]
pub fn class_of(tag: u32) -> Option<TagClass> {
    lookup(tag).map(|e| e.2)
}

/// How many tags the format defines.
#[must_use]
pub fn defined_tag_count() -> usize {
    TAG_TABLE.len()
}

fn lookup(tag: u32) -> Option<&'static (u32, &'static str, TagClass)> {
    let idx = TAG_TABLE.binary_search_by_key(&tag, |e| e.0).ok()?;
    TAG_TABLE.get(idx)
}

/// The atomic and essential tag lists a file declares about itself.
#[derive(Clone, Debug, Default)]
pub struct TagPolicy {
    atomic: BTreeSet<u32>,
    essential: BTreeSet<u32>,
}

impl TagPolicy {
    /// The cap on how many tags either list may hold.
    ///
    /// The lists are `size / 4` entries of a record whose size a hostile file
    /// chooses, so they need a bound like everything else. The corpus uses
    /// twelve atomic tags and no essential ones.
    pub const MAX_TAGS: usize = 4096;

    /// An empty policy: nothing atomic, nothing essential.
    #[must_use]
    pub fn new() -> TagPolicy {
        TagPolicy::default()
    }

    /// Unions one `TAG_ATOMICTAGS` payload into the atomic list.
    pub fn absorb_atomic(&mut self, payload: &[u8]) {
        absorb(&mut self.atomic, payload);
    }

    /// Unions one `TAG_ESSENTIALTAGS` payload into the essential list.
    pub fn absorb_essential(&mut self, payload: &[u8]) {
        absorb(&mut self.essential, payload);
    }

    /// Whether a tag is on the atomic list.
    #[must_use]
    pub fn is_atomic(&self, tag: u32) -> bool {
        self.atomic.contains(&tag)
    }

    /// Whether a tag is on the essential list.
    #[must_use]
    pub fn is_essential(&self, tag: u32) -> bool {
        self.essential.contains(&tag)
    }

    /// The atomic list, sorted.
    pub fn atomic(&self) -> impl Iterator<Item = u32> + '_ {
        self.atomic.iter().copied()
    }

    /// The essential list, sorted.
    pub fn essential(&self) -> impl Iterator<Item = u32> + '_ {
        self.essential.iter().copied()
    }

    /// The three-way decision for a tag with no handler.
    #[must_use]
    pub fn unknown_action(&self, tag: u32) -> UnknownAction {
        if self.is_essential(tag) {
            UnknownAction::Abort
        } else if self.is_atomic(tag) {
            UnknownAction::StripSubtree
        } else {
            UnknownAction::Skip
        }
    }
}

fn absorb(into: &mut BTreeSet<u32>, payload: &[u8]) {
    for chunk in payload.as_chunks::<4>().0.iter() {
        if into.len() >= TagPolicy::MAX_TAGS {
            return;
        }
        into.insert(u32::from_le_bytes(*chunk));
    }
}

/// A `.xar` `REFERENCE`.
///
/// The format's only pointer is the record number, and a reference is a
/// signed `i32` over it: positive is a record number, negative is one of the
/// built-in objects the format predefines (colours, arrowheads, dash
/// patterns, units, the default bitmap), and zero means null — the original's
/// writer returns 0 when it fails to resolve something.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum Ref {
    /// Null, or the writer failed.
    None,
    /// A predefined built-in, identified by its negative value.
    Builtin(i32),
    /// The number of the record that defines the target.
    Record(u32),
}

impl Ref {
    /// Classifies a raw reference value.
    #[must_use]
    pub const fn parse(v: i32) -> Ref {
        if v == 0 {
            Ref::None
        } else if v < 0 {
            Ref::Builtin(v)
        } else {
            Ref::Record(v as u32)
        }
    }

    /// The raw value this reference came from.
    #[must_use]
    pub const fn raw(self) -> i32 {
        match self {
            Ref::None => 0,
            Ref::Builtin(v) => v,
            Ref::Record(n) => n as i32,
        }
    }
}

/// The predefined unit a negative unit reference names
/// (`research/01 §5.2`).
#[must_use]
pub const fn builtin_unit_name(reference: i32) -> Option<&'static str> {
    let name = match reference {
        -1 => "untyped",
        -2 => "millimetres",
        -3 => "centimetres",
        -4 => "metres",
        -5 => "kilometres",
        -6 => "millipoints",
        -7 => "points",
        -8 => "picas",
        -9 => "inches",
        -10 => "feet",
        -11 => "yards",
        -12 => "miles",
        -13 => "pixels",
        _ => return None,
    };
    Some(name)
}

/// The predefined arrowhead a negative reference names
/// (`research/01 §6.3`).
#[must_use]
pub const fn builtin_arrow_name(reference: i32) -> Option<&'static str> {
    let name = match reference {
        -1 => "none",
        -2 => "straight",
        -3 => "angled",
        -4 => "rounded",
        -5 => "dot",
        -6 => "diamond",
        -7 => "feather",
        -8 => "feather 2",
        -9 => "hollow diamond",
        _ => return None,
    };
    Some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_sorted_and_complete() {
        // 300 from the master table of `research/01 §4.1`, plus the two it
        // omits: TAG_PATH_FLAGS (111) and TAG_TEXT_FONT_SIZE (2906).
        assert_eq!(TAG_TABLE.len(), 302);
        assert!(TAG_TABLE.windows(2).all(|w| w[0].0 < w[1].0));
    }

    #[test]
    fn known_tags_resolve() {
        assert_eq!(name_of(TAG_UP), Some("TAG_UP"));
        assert_eq!(name_of(4207), Some("TAG_TEXT_STORY_TRANSLATION_INFO"));
        assert_eq!(name_of(99_999), None);
        assert_eq!(name_of(111), Some("TAG_PATH_FLAGS"));
        assert_eq!(name_of(2906), Some("TAG_TEXT_FONT_SIZE"));
        assert_eq!(class_of(TAG_DOWN), Some(TagClass::Structural));
        assert_eq!(class_of(116), Some(TagClass::Object));
        assert_eq!(class_of(150), Some(TagClass::Attribute));
        assert_eq!(class_of(51), Some(TagClass::Definition));
    }

    #[test]
    fn the_atomic_list_is_a_union_of_many_records() {
        let mut p = TagPolicy::new();
        p.absorb_atomic(&4052u32.to_le_bytes());
        p.absorb_atomic(&4057u32.to_le_bytes());
        assert!(p.is_atomic(4052));
        assert!(p.is_atomic(4057));
        assert!(!p.is_atomic(4050));
        assert_eq!(p.unknown_action(4052), UnknownAction::StripSubtree);
        assert_eq!(p.unknown_action(9999), UnknownAction::Skip);
        p.absorb_essential(&9999u32.to_le_bytes());
        assert_eq!(p.unknown_action(9999), UnknownAction::Abort);
    }

    #[test]
    fn a_partial_trailing_entry_is_ignored() {
        let mut p = TagPolicy::new();
        p.absorb_atomic(&[1, 0, 0, 0, 2, 0]);
        assert!(p.is_atomic(1));
        assert_eq!(p.atomic().count(), 1);
    }

    #[test]
    fn the_tag_list_cannot_grow_without_bound() {
        let mut p = TagPolicy::new();
        let payload: Vec<u8> = (0u32..100_000).flat_map(u32::to_le_bytes).collect();
        p.absorb_atomic(&payload);
        assert_eq!(p.atomic().count(), TagPolicy::MAX_TAGS);
    }

    #[test]
    fn references_classify() {
        assert_eq!(Ref::parse(0), Ref::None);
        assert_eq!(Ref::parse(-2), Ref::Builtin(-2));
        assert_eq!(Ref::parse(31), Ref::Record(31));
        assert_eq!(Ref::parse(31).raw(), 31);
    }
}
