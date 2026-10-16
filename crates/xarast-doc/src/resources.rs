//! The document's resource tables.
//!
//! Everything the original kept as a `DocComponent` leaves the tree and lives
//! here, behind an [`Arc`] so that cloning a node — for undo, for the
//! clipboard, for a blend step — copies a handful of pointers.
//!
//! Two rules that are easy to get wrong:
//!
//! - **Deduplication is by SHA-256 of the decoded payload**, so importing the
//!   same bitmap twice yields one resource and one allocation.
//! - **Liveness is a sweep, not a reference count.** `Arc` counts references;
//!   whether a resource is still *wanted* is [`collect_unused`], run on save
//!   and on history eviction and never per edit — because a node detached by
//!   undo still holds its resources and must keep them.

use std::collections::HashMap;
use std::sync::Arc;

use sha2::{Digest, Sha256};
use slotmap::SlotMap;

use crate::kind::{ArrowSpec, NodeKind};

slotmap::new_key_type! {
    /// A bitmap resource.
    pub struct BitmapId;
    /// A dash pattern resource.
    pub struct DashId;
    /// An arrowhead resource.
    pub struct ArrowId;
}

/// A reference from the tree into a resource table.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, serde::Serialize, serde::Deserialize)]
pub enum ResourceRef {
    /// A palette colour.
    Colour(xarast_color::ColourId),
    /// A bitmap.
    Bitmap(BitmapId),
    /// A dash pattern.
    Dash(DashId),
    /// An arrowhead.
    Arrow(ArrowId),
}

/// A font file a document carries for display only: one face of a
/// `.xarast` package's `resources/fonts/` (`research/06 §6.7` rule 2),
/// as its `@font-face` rule names it.
///
/// The model never draws with it or interprets it: the application lays
/// the document's text out with these faces only where the machine lacks
/// the face itself (`docs/memory/text.md`, "Embedded fonts on read").
#[derive(Clone, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub struct EmbeddedFont {
    /// The family the face was drawn as (its `@font-face` `font-family`).
    pub family: Arc<str>,
    /// CSS weight the rule declares.
    pub weight: u16,
    /// Whether the rule declares an italic (or oblique) style.
    pub italic: bool,
    /// The file as stored: WOFF2, or a plain OpenType file.
    #[serde(with = "crate::journal::blob")]
    pub data: Arc<[u8]>,
}

/// How a bitmap's pixels are laid out.
#[derive(
    Copy, Clone, PartialEq, Eq, Debug, Hash, Default, serde::Serialize, serde::Deserialize,
)]
pub struct BitmapInfo {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Bits per pixel: 1, 2, 4, 8, 24 or 32.
    pub bpp: u8,
    /// Horizontal resolution in dots per inch.
    pub dpi_x: u32,
    /// Vertical resolution in dots per inch.
    pub dpi_y: u32,
}

/// A bitmap's decoded pixels, plus its palette when it has one.
///
/// Decoding is `xarast-image`'s job; this crate only carries the bytes.
#[derive(Clone, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct BitmapData {
    /// The pixels, row-major, top row first.
    #[serde(with = "crate::journal::blob")]
    pub pixels: Arc<[u8]>,
    /// The palette, for indexed bitmaps.
    pub palette: Arc<[xarast_color::Rgba8]>,
}

/// The container format encoded bytes arrived in.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
#[repr(u8)]
#[derive(serde::Serialize, serde::Deserialize)]
pub enum ImageFormat {
    /// PNG.
    Png,
    /// JPEG.
    Jpeg,
    /// Windows BMP or DIB.
    Bmp,
    /// GIF.
    Gif,
    /// Something we did not recognise; the bytes are kept regardless.
    Unknown,
}

/// The encoded source bytes of a bitmap.
///
/// Keeping them is a fidelity requirement, not an optimisation: re-encoding an
/// embedded JPEG on save loses quality on every round trip. If the bytes are
/// here, the writer emits them verbatim.
#[derive(Clone, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub struct OriginalEncoded {
    /// The container format.
    pub format: ImageFormat,
    /// The bytes, exactly as they arrived.
    #[serde(with = "crate::journal::blob")]
    pub bytes: Arc<[u8]>,
}

/// A bitmap that is generated rather than stored.
///
/// Not serialised: regenerated instead. Its cache key is the parameters
/// themselves.
#[derive(Clone, PartialEq, Debug, serde::Serialize, serde::Deserialize)]
pub struct ProceduralSource {
    /// The generator's parameters.
    pub params: crate::fill::ProceduralParams,
    /// Whether it is a fractal rather than plain noise.
    pub fractal: bool,
}

impl ProceduralSource {
    /// The key a generated bitmap is cached under: SHA-256 of every
    /// parameter (floats by their bits), the fractal flag, and the name
    /// and version of the generator, so that a change to the generator
    /// can never serve an old bitmap under a new algorithm (phase 13 H5,
    /// H6; the `IsSameAsCachedFractal` equivalent).
    #[must_use]
    pub fn cache_key(&self) -> [u8; 32] {
        use crate::digest::{Canon, CanonicalHasher};
        let mut h = CanonicalHasher::new();
        h.str(PROCEDURAL_GENERATOR);
        h.bool(self.fractal);
        self.params.canon(&mut h);
        h.finish()
    }
}

/// The generator [`ProceduralSource::cache_key`] names. Bump the version
/// whenever the generated pixels change for the same parameters.
pub const PROCEDURAL_GENERATOR: &str = "xarast-procedural/0";

/// One bitmap in the document.
#[derive(Clone, PartialEq, Debug, serde::Serialize, serde::Deserialize)]
pub struct BitmapResource {
    /// The name shown in the bitmap gallery.
    pub name: Arc<str>,
    /// The pixel layout.
    pub info: BitmapInfo,
    /// The decoded pixels, copy-on-write.
    pub pixels: Arc<BitmapData>,
    /// The encoded source bytes, when we have them.
    pub original: Option<Arc<OriginalEncoded>>,
    /// The generator, when the bitmap is procedural.
    pub procedural: Option<ProceduralSource>,
    /// The palette index that is transparent, GIF style.
    pub transparent_index: Option<u8>,
}

impl BitmapResource {
    /// The SHA-256 of the payload: the deduplication key.
    ///
    /// It covers the decoded pixels **and the encoded original**. Two
    /// bitmaps that decode to the same pixels but arrived as different bytes
    /// are still two resources, because the writer emits `original`
    /// verbatim and collapsing them would silently discard one of the two
    /// encodings. It also matters while a decoder does not exist yet: the
    /// `.xar` importer stores the encoded bytes and leaves `pixels` empty
    /// until Phase 10, and without the original in the hash every bitmap in
    /// a file would deduplicate onto the first.
    #[must_use]
    pub fn content_hash(&self) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(self.info.width.to_le_bytes());
        h.update(self.info.height.to_le_bytes());
        h.update([self.info.bpp]);
        h.update(&*self.pixels.pixels);
        for c in self.pixels.palette.iter() {
            h.update([c.r, c.g, c.b, c.a]);
        }
        match &self.original {
            Some(o) => {
                h.update([1u8, o.format as u8]);
                h.update(&*o.bytes);
            }
            None => h.update([0u8]),
        }
        h.finalize().into()
    }
}

/// The bitmap table: resources in insertion order under keys that are
/// derived from their content, not handed out by a slot allocator.
///
/// The key is what makes insertion undoable exactly: a slot map cannot put
/// a resource back under the key it had, so undo of a placement could only
/// hide it, and the digest would have to know about tombstones. Here undo
/// removes the entry, redo puts it back under the same key, and a key is a
/// pure function of the table's state, so a journal replay allocates the
/// same ones as the live document did.
#[derive(Clone, Debug, Default)]
pub(crate) struct BitmapTable {
    pub(crate) items: Vec<(BitmapId, BitmapResource)>,
    index: HashMap<BitmapId, usize>,
}

impl BitmapTable {
    pub(crate) fn from_items(items: Vec<(BitmapId, BitmapResource)>) -> BitmapTable {
        let mut t = BitmapTable {
            items,
            index: HashMap::new(),
        };
        t.reindex();
        t
    }

    fn reindex(&mut self) {
        self.index = self
            .items
            .iter()
            .enumerate()
            .map(|(i, (id, _))| (*id, i))
            .collect();
    }

    /// A key not in use, derived from the content hash and then probing.
    fn free_id(&self, hash: &[u8; 32]) -> BitmapId {
        let mut idx = u32::from_le_bytes([hash[0], hash[1], hash[2], hash[3]]);
        loop {
            // Version 1 (occupied, as a slot map spells it); the index
            // `u32::MAX` is the null key.
            if idx != u32::MAX {
                let id = BitmapId::from(slotmap::KeyData::from_ffi((1u64 << 32) | u64::from(idx)));
                if !self.index.contains_key(&id) {
                    return id;
                }
            }
            idx = idx.wrapping_add(1);
        }
    }

    fn push(&mut self, id: BitmapId, res: BitmapResource) {
        self.index.insert(id, self.items.len());
        self.items.push((id, res));
    }

    fn take(&mut self, id: BitmapId) -> Option<BitmapResource> {
        let i = self.index.remove(&id)?;
        let (_, res) = self.items.remove(i);
        if i < self.items.len() {
            self.reindex();
        }
        Some(res)
    }

    fn contains(&self, id: BitmapId) -> bool {
        self.index.contains_key(&id)
    }

    fn get(&self, id: BitmapId) -> Option<&BitmapResource> {
        self.index.get(&id).map(|i| &self.items[*i].1)
    }

    fn get_mut(&mut self, id: BitmapId) -> Option<&mut BitmapResource> {
        let i = *self.index.get(&id)?;
        Some(&mut self.items[i].1)
    }

    fn iter(&self) -> impl Iterator<Item = (BitmapId, &BitmapResource)> + '_ {
        self.items.iter().map(|(id, r)| (*id, r))
    }

    fn len(&self) -> usize {
        self.items.len()
    }

    fn retain(&mut self, mut keep: impl FnMut(BitmapId) -> bool) {
        self.items.retain(|(id, _)| keep(*id));
        self.reindex();
    }
}

/// Every resource the document owns.
#[derive(Clone, Debug, Default)]
pub struct DocumentResources {
    /// Named and indexed colours.
    pub colours: xarast_color::ColourTable,
    bitmaps: BitmapTable,
    dashes: SlotMap<DashId, xarast_geom::DashPattern>,
    arrows: SlotMap<ArrowId, ArrowSpec>,
    bitmap_by_hash: HashMap<[u8; 32], BitmapId>,
    /// Faces carried for display, in the order first seen.
    fonts: Vec<EmbeddedFont>,
    /// Bumped by every change to the bitmap, dash, arrow and font tables
    /// that does not go through a transaction (an insertion, a sweep), so
    /// that the operation journal can tell when to record them.
    tables_rev: u64,
}

impl DocumentResources {
    /// An empty set of tables.
    #[must_use]
    pub fn new() -> DocumentResources {
        DocumentResources::default()
    }

    /// Inserts a bitmap, deduplicating by the SHA-256 of its decoded payload.
    ///
    /// Inserting the same image twice returns the same [`BitmapId`] and keeps
    /// one allocation.
    pub fn insert_bitmap(&mut self, res: BitmapResource) -> BitmapId {
        let hash = res.content_hash();
        if let Some(id) = self.bitmap_by_hash.get(&hash) {
            return *id;
        }
        let id = self.bitmaps.free_id(&hash);
        self.bitmaps.push(id, res);
        self.bitmap_by_hash.insert(hash, id);
        self.tables_rev += 1;
        id
    }

    /// The key a bitmap with this content would be inserted under, and the
    /// bitmap already holding the content, if any. Pure: a transaction
    /// uses it to name the key in the action it records.
    pub(crate) fn plan_bitmap(&self, res: &BitmapResource) -> (BitmapId, bool) {
        let hash = res.content_hash();
        match self.bitmap_by_hash.get(&hash) {
            Some(id) => (*id, true),
            None => (self.bitmaps.free_id(&hash), false),
        }
    }

    /// Adds a bitmap under a key chosen by [`Self::plan_bitmap`]
    /// ([`crate::Action::InsertBitmap`]). `None` when the key is taken.
    /// Not counted in [`DocumentResources::tables_rev`]: the journal
    /// records the action.
    pub(crate) fn add_bitmap_at(&mut self, id: BitmapId, res: BitmapResource) -> Option<()> {
        if self.bitmaps.contains(id) {
            return None;
        }
        self.bitmap_by_hash.entry(res.content_hash()).or_insert(id);
        self.bitmaps.push(id, res);
        Some(())
    }

    /// Takes a bitmap out ([`crate::Action::RemoveBitmap`]), returning it.
    /// Not counted in [`DocumentResources::tables_rev`].
    pub(crate) fn take_bitmap(&mut self, id: BitmapId) -> Option<BitmapResource> {
        let res = self.bitmaps.take(id)?;
        let hash = res.content_hash();
        if self.bitmap_by_hash.get(&hash) == Some(&id) {
            self.bitmap_by_hash.remove(&hash);
        }
        Some(res)
    }

    /// Changes whenever the bitmap, dash, arrow or font table changes
    /// outside a transaction. The colour table is not covered: it changes
    /// only through [`crate::Action::SetPalette`].
    #[must_use]
    pub fn tables_rev(&self) -> u64 {
        self.tables_rev
    }

    /// The tables [`DocumentResources::tables_rev`] covers, cloned (every
    /// payload is behind an `Arc`).
    pub(crate) fn tables(&self) -> crate::journal::Tables {
        crate::journal::Tables {
            bitmaps: self.bitmaps.items.clone(),
            dashes: self.dashes.clone(),
            arrows: self.arrows.clone(),
            fonts: self.fonts.clone(),
        }
    }

    /// Replaces the tables [`DocumentResources::tables_rev`] covers, keys
    /// and all, rebuilding the deduplication index.
    pub(crate) fn set_tables(&mut self, t: crate::journal::Tables) {
        self.bitmap_by_hash = t
            .bitmaps
            .iter()
            .map(|(id, r)| (r.content_hash(), *id))
            .collect();
        self.bitmaps = BitmapTable::from_items(t.bitmaps);
        self.dashes = t.dashes;
        self.arrows = t.arrows;
        self.fonts = t.fonts;
        self.tables_rev += 1;
    }

    /// Looks a bitmap up.
    #[inline]
    #[must_use]
    pub fn bitmap(&self, id: BitmapId) -> Option<&BitmapResource> {
        self.bitmaps.get(id)
    }

    /// Replaces a bitmap's pixels, returning the previous ones.
    pub(crate) fn replace_bitmap_pixels(
        &mut self,
        id: BitmapId,
        pixels: Option<Arc<BitmapData>>,
    ) -> Option<Arc<BitmapData>> {
        let res = self.bitmaps.get_mut(id)?;
        let old = res.pixels.clone();
        if let Some(p) = pixels {
            let old_hash = res.content_hash();
            res.pixels = p;
            let new_hash = res.content_hash();
            self.bitmap_by_hash.remove(&old_hash);
            self.bitmap_by_hash.insert(new_hash, id);
        }
        Some(old)
    }

    /// Puts `new` in the place of bitmap `id`, keeping the key, and returns
    /// what was there. The deduplication index follows: the old content's
    /// entry goes (when it named `id`), the new content's is added unless
    /// another bitmap already holds that content.
    ///
    /// Not counted in [`DocumentResources::tables_rev`]: the change goes
    /// through [`crate::Action::ReplaceBitmap`], which the journal records.
    pub(crate) fn swap_bitmap(
        &mut self,
        id: BitmapId,
        new: Arc<BitmapResource>,
    ) -> Option<BitmapResource> {
        let slot = self.bitmaps.get_mut(id)?;
        let old_hash = slot.content_hash();
        let old = std::mem::replace(slot, (*new).clone());
        let new_hash = slot.content_hash();
        if self.bitmap_by_hash.get(&old_hash) == Some(&id) {
            self.bitmap_by_hash.remove(&old_hash);
        }
        self.bitmap_by_hash.entry(new_hash).or_insert(id);
        Some(old)
    }

    /// Every bitmap, in slot order.
    pub fn bitmaps(&self) -> impl Iterator<Item = (BitmapId, &BitmapResource)> + '_ {
        self.bitmaps.iter()
    }

    /// Inserts a dash pattern.
    pub fn insert_dash(&mut self, d: xarast_geom::DashPattern) -> DashId {
        self.tables_rev += 1;
        self.dashes.insert(d)
    }

    /// Looks a dash pattern up.
    #[inline]
    #[must_use]
    pub fn dash(&self, id: DashId) -> Option<&xarast_geom::DashPattern> {
        self.dashes.get(id)
    }

    /// Every dash pattern, in slot order.
    pub fn dashes(&self) -> impl Iterator<Item = (DashId, &xarast_geom::DashPattern)> + '_ {
        self.dashes.iter()
    }

    /// Inserts an arrowhead.
    pub fn insert_arrow(&mut self, a: ArrowSpec) -> ArrowId {
        self.tables_rev += 1;
        self.arrows.insert(a)
    }

    /// Looks an arrowhead up.
    #[inline]
    #[must_use]
    pub fn arrow(&self, id: ArrowId) -> Option<&ArrowSpec> {
        self.arrows.get(id)
    }

    /// Every arrowhead, in slot order.
    pub fn arrows(&self) -> impl Iterator<Item = (ArrowId, &ArrowSpec)> + '_ {
        self.arrows.iter()
    }

    /// Adds a face carried for display. The same family with the same
    /// bytes is kept once; returns whether it was new.
    pub fn insert_font(&mut self, font: EmbeddedFont) -> bool {
        if self
            .fonts
            .iter()
            .any(|f| f.family == font.family && f.data == font.data)
        {
            return false;
        }
        self.fonts.push(font);
        self.tables_rev += 1;
        true
    }

    /// The faces carried for display, in the order first seen.
    #[must_use]
    pub fn fonts(&self) -> &[EmbeddedFont] {
        &self.fonts
    }

    /// Whether a reference resolves.
    #[must_use]
    pub fn contains(&self, r: ResourceRef) -> bool {
        match r {
            ResourceRef::Colour(id) => self.colours.get(id).is_some(),
            ResourceRef::Bitmap(id) => self.bitmaps.contains(id),
            ResourceRef::Dash(id) => self.dashes.contains_key(id),
            ResourceRef::Arrow(id) => self.arrows.contains_key(id),
        }
    }

    /// How many resources of each kind there are: bitmaps, dashes, arrows.
    #[must_use]
    pub fn counts(&self) -> (usize, usize, usize) {
        (self.bitmaps.len(), self.dashes.len(), self.arrows.len())
    }

    /// The deduplication key a bitmap was stored under
    /// ([`BitmapResource::content_hash`]), kept from its insertion so that
    /// nobody has to hash its bytes again: the bitmap gallery keys its
    /// thumbnails on it. `None` for an unknown id.
    #[must_use]
    pub fn bitmap_key(&self, id: BitmapId) -> Option<[u8; 32]> {
        self.bitmap_by_hash
            .iter()
            .find_map(|(h, b)| (*b == id).then_some(*h))
    }

    fn retain_bitmaps(&mut self, keep: &std::collections::HashSet<BitmapId>) -> usize {
        let before = self.bitmaps.len();
        self.bitmaps.retain(|id| keep.contains(&id));
        self.bitmap_by_hash
            .retain(|_, id| self.bitmaps.contains(*id));
        let gone = before - self.bitmaps.len();
        if gone > 0 {
            self.tables_rev += 1;
        }
        gone
    }
}

/// Collects every resource reference the node makes.
pub(crate) fn refs_of(kind: &NodeKind, out: &mut Vec<ResourceRef>) {
    match kind {
        NodeKind::Bitmap(b) => out.push(ResourceRef::Bitmap(b.image)),
        NodeKind::Guideline(g) => {
            if let Some(c) = g.colour {
                out.push(ResourceRef::Colour(c));
            }
        }
        NodeKind::Layer(l) => {
            if let Some(c) = l.guide_colour {
                out.push(ResourceRef::Colour(c));
            }
        }
        NodeKind::Attr(a) => a.value.resource_refs(out),
        _ => {}
    }
}

/// Whether a node refers to bitmap `id`: a bitmap object, or a fill or
/// transparency attribute.
pub(crate) fn uses_bitmap(kind: &NodeKind, id: BitmapId) -> bool {
    let mut refs = Vec::new();
    refs_of(kind, &mut refs);
    refs.contains(&ResourceRef::Bitmap(id))
}

/// Sweeps resources that nothing alive in the arena references.
///
/// "Alive" includes nodes the history has detached but still retains, which is
/// exactly why this is a sweep and not a reference count: undoing a deletion
/// must find the resources still there.
///
/// Returns how many resources went.
pub fn collect_unused(doc: &mut crate::Document) -> usize {
    let mut keep_bitmaps: std::collections::HashSet<BitmapId> = std::collections::HashSet::new();
    let mut refs = Vec::new();
    for (_, data) in doc.tree.iter() {
        refs.clear();
        refs_of(&data.kind, &mut refs);
        for r in &refs {
            if let ResourceRef::Bitmap(b) = r {
                keep_bitmaps.insert(*b);
            }
        }
    }
    doc.resources.retain_bitmaps(&keep_bitmaps)
}

/// How the document uses one bitmap: the bitmap gallery's "uses"
/// (`phase-10` T10.7.1).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct BitmapUsage {
    /// References from the document as it stands — bitmap objects, and
    /// fill or transparency attributes — reachable from the root.
    pub live: u32,
    /// References from nodes only the undo history still holds (a deleted
    /// object, an undone placement). While there is one the resource must
    /// stay: an undo or a redo brings the reference back.
    pub retained: u32,
}

impl BitmapUsage {
    /// Whether anything at all refers to the bitmap, the history included.
    #[must_use]
    pub fn in_use(self) -> bool {
        self.live > 0 || self.retained > 0
    }
}

/// Counts every reference to every bitmap, split into the document's own
/// ([`BitmapUsage::live`]) and the history's ([`BitmapUsage::retained`]).
/// A bitmap nothing refers to is listed with zero counts. One pass over the
/// arena plus one over the live tree.
#[must_use]
pub fn bitmap_usage(doc: &crate::Document) -> HashMap<BitmapId, BitmapUsage> {
    let mut out: HashMap<BitmapId, BitmapUsage> = doc
        .resources
        .bitmaps()
        .map(|(id, _)| (id, BitmapUsage::default()))
        .collect();
    let live: std::collections::HashSet<crate::tree::NodeId> =
        doc.tree.preorder(doc.tree.root()).collect();
    let mut refs = Vec::new();
    for (id, data) in doc.tree.iter() {
        refs.clear();
        refs_of(&data.kind, &mut refs);
        for r in &refs {
            if let ResourceRef::Bitmap(b) = r {
                let u = out.entry(*b).or_default();
                if live.contains(&id) {
                    u.live += 1;
                } else {
                    u.retained += 1;
                }
            }
        }
    }
    out
}

/// Why a bitmap could not be removed.
#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
pub enum RemoveBitmapError {
    /// No such bitmap.
    #[error("no such bitmap")]
    Missing,
    /// Something refers to it: an object, a fill, or a step of the undo
    /// history.
    #[error("the bitmap is in use")]
    InUse(BitmapUsage),
}

/// Removes one bitmap that nothing refers to — the bitmap gallery's
/// Delete, which is offered for unused bitmaps only (`phase-10`
/// acceptance 15). Like [`collect_unused`] it is not an undoable edit: an
/// unreferenced resource draws nothing, and the save-time sweep would drop
/// it anyway.
///
/// # Errors
///
/// [`RemoveBitmapError::InUse`] when an object, an attribute or a node the
/// history retains still refers to it; the document is then unchanged.
pub fn remove_unused_bitmap(
    doc: &mut crate::Document,
    id: BitmapId,
) -> Result<(), RemoveBitmapError> {
    if doc.resources.bitmap(id).is_none() {
        return Err(RemoveBitmapError::Missing);
    }
    let usage = bitmap_usage(doc).get(&id).copied().unwrap_or_default();
    if usage.in_use() {
        return Err(RemoveBitmapError::InUse(usage));
    }
    let keep: std::collections::HashSet<BitmapId> = doc
        .resources
        .bitmaps()
        .map(|(b, _)| b)
        .filter(|b| *b != id)
        .collect();
    doc.resources.retain_bitmaps(&keep);
    Ok(())
}
