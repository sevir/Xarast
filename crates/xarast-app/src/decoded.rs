//! A document's decoded bitmaps, shared by every walker of it
//! (XARA-T-0281).
//!
//! A [`SceneWalker`](crate::SceneWalker) registers each bitmap once, but
//! export, thumbnails and [`build_scene`](crate::build_scene) each make a
//! fresh walker, and each used to decode every bitmap of the document
//! again. A [`Session`](crate::Session) owns one [`DecodedImages`] and
//! hands it to every walker it makes; a walker given one looks a bitmap up
//! there before decoding it and files what it decodes.
//!
//! # The key is the resource's identity, not its content
//!
//! An entry is found by the addresses of the resource's `Arc`s — its
//! pixel data and its encoded original — plus its declared size and the
//! [`PixelBudget`] the image is registered under. The entry keeps clones
//! of those `Arc`s, so an address cannot be reused by another resource
//! while the entry lives. The document's resources are copy-on-write:
//! a bitmap whose bytes change gets new `Arc`s, so it misses and is
//! decoded afresh. Two resources with equal bytes but separate `Arc`s are
//! decoded twice, which only costs time.
//!
//! This is why the damage rule still holds. `scene_damage` compares the
//! images two scenes point to by [`ImageRef`] equality, which is pointer
//! equality of the shared store or else equal pixels — never this cache.
//! A hit hands out the very `ImageRef` an earlier walk registered, which
//! is equal to itself; a changed resource misses, gets a new `ImageRef`
//! with its own pixels, and compares by content.
//!
//! # Who files here
//!
//! Every walker given the cache, and the bitmap gallery's thumbnail
//! thread (`DecodedImages::image_for`), which decodes through the same
//! path and under the process-wide budget the session's walkers use. So a
//! bitmap is decoded once whichever of the two sees it first; two that
//! miss at the same moment both decode, and the first filed wins.
//!
//! # Lifetime
//!
//! Entries whose resource is no longer in the document are dropped the
//! next time a walker of it registers images, so a deleted bitmap's
//! pixels go with the last scene that used them. The decoded levels stay
//! under the pixel budget like any other image.
//!
//! # Photo-adjusted images
//!
//! A bitmap object with photo operations (`xarast_doc::photo`) shows a
//! **derived** image: the master put through the chain. Those are filed
//! here too, keyed by the master's key plus the hash of the evaluable
//! chain ([`xarast_doc::PhotoOps::hash`]), so that export and thumbnail
//! walkers evaluate a chain the view already evaluated no more than they
//! decode. A derived entry goes when its master does, and when no object
//! attached to the document uses its chain any more
//! (`DecodedImages::retain_derived`, which the walker calls when the
//! document changes): an undo that brings a chain back evaluates it
//! again.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};

use xarast_doc::resources::{BitmapData, BitmapResource, OriginalEncoded};
use xarast_render::{ImageRef, PixelBudget};

/// How a resource is recognised: see the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Key {
    pixels: usize,
    original: usize,
    width: u32,
    height: u32,
    budget: usize,
}

impl Key {
    fn of(res: &BitmapResource, budget: &Arc<PixelBudget>) -> Key {
        Key {
            pixels: Arc::as_ptr(&res.pixels) as usize,
            original: res.original.as_ref().map_or(0, |o| Arc::as_ptr(o) as usize),
            width: res.info.width,
            height: res.info.height,
            budget: Arc::as_ptr(budget) as usize,
        }
    }

    fn resource(&self) -> (usize, usize, u32, u32) {
        (self.pixels, self.original, self.width, self.height)
    }
}

struct Entry {
    /// Held so that the addresses in the key stay this resource's.
    _pixels: Arc<BitmapData>,
    _original: Option<Arc<OriginalEncoded>>,
    _budget: Arc<PixelBudget>,
    /// `None`: the decode failed, and is not tried again.
    image: Option<ImageRef>,
}

#[derive(Default)]
struct Inner {
    entries: HashMap<Key, Entry>,
    /// Derived images, by master and chain hash.
    derived: HashMap<(Key, [u8; 32]), Entry>,
    stats: DecodedImagesStats,
}

/// What a [`DecodedImages`] has done so far. Counters only grow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DecodedImagesStats {
    /// Bitmaps a walker found here instead of decoding.
    pub hits: u64,
    /// Bitmaps decoded (or copied, for native pixels) and filed.
    pub decoded: u64,
    /// Entries dropped because their resource left the document.
    pub pruned: u64,
    /// Photo-adjusted images a walker found here instead of evaluating.
    pub derived_hits: u64,
    /// Photo-adjusted images evaluated and filed.
    pub derived: u64,
    /// Photo-adjusted images dropped: their master left the document or
    /// no attached object uses their chain any more.
    pub derived_pruned: u64,
}

/// The decoded bitmaps of one document; cheap to clone (the clones share
/// it). See the module docs.
#[derive(Clone, Default)]
pub struct DecodedImages {
    inner: Arc<Mutex<Inner>>,
}

impl std::fmt::Debug for DecodedImages {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let i = self.lock();
        f.debug_struct("DecodedImages")
            .field("entries", &i.entries.len())
            .field("stats", &i.stats)
            .finish()
    }
}

impl DecodedImages {
    /// An empty cache.
    #[must_use]
    pub fn new() -> DecodedImages {
        DecodedImages::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // Plain data, consistent at every step: carry on after a panic.
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The counters so far.
    #[must_use]
    pub fn stats(&self) -> DecodedImagesStats {
        self.lock().stats
    }

    /// How many bitmaps are held (failures included).
    #[must_use]
    pub fn len(&self) -> usize {
        self.lock().entries.len()
    }

    /// Whether nothing is held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lock().entries.is_empty()
    }

    /// The image filed for `res` under `budget`: `Some(Some(_))` a
    /// decoded image, `Some(None)` a decode that failed, `None` never
    /// seen.
    pub(crate) fn get(
        &self,
        res: &BitmapResource,
        budget: &Arc<PixelBudget>,
    ) -> Option<Option<ImageRef>> {
        let mut i = self.lock();
        let found = i
            .entries
            .get(&Key::of(res, budget))
            .map(|e| e.image.clone());
        if found.is_some() {
            i.stats.hits += 1;
        }
        found
    }

    /// Files what a walker made of `res`, returning what is filed: the
    /// first image filed wins, so that two walkers that decoded the same
    /// bitmap at once end up sharing one `ImageRef`.
    pub(crate) fn insert(
        &self,
        res: &BitmapResource,
        budget: &Arc<PixelBudget>,
        image: Option<ImageRef>,
    ) -> Option<ImageRef> {
        let mut i = self.lock();
        i.stats.decoded += 1;
        let e = i
            .entries
            .entry(Key::of(res, budget))
            .or_insert_with(|| Entry {
                _pixels: Arc::clone(&res.pixels),
                _original: res.original.clone(),
                _budget: Arc::clone(budget),
                image,
            });
        e.image.clone()
    }

    /// The image of `res` under `budget`: the one filed, or else decoded
    /// now by the walker's decode path ([`crate::walker::ready_image`]) and
    /// filed, failures included. Blocking; for a worker thread (the
    /// bitmap gallery's thumbnails, XARA-T-0293). A resource no walker
    /// would decode (neither native pixels nor an encoded original) is
    /// `None` and is not filed, so a walker still counts it as pending.
    /// A panicking decoder is a failed decode, as in the walker.
    pub(crate) fn image_for(
        &self,
        res: &BitmapResource,
        budget: &Arc<PixelBudget>,
    ) -> Option<ImageRef> {
        if !crate::walker::is_decodable(res) {
            return None;
        }
        if let Some(found) = self.get(res, budget) {
            return found;
        }
        // A panicking decoder is a missing image, not a crash.
        let made = crate::crash::expect_panics(|| crate::walker::ready_image(res, budget))
            .ok()
            .flatten();
        self.insert(res, budget, made)
    }

    /// The derived image filed for `res` under the chain hashed `ops`:
    /// as [`DecodedImages::get`].
    pub(crate) fn get_derived(
        &self,
        res: &BitmapResource,
        budget: &Arc<PixelBudget>,
        ops: [u8; 32],
    ) -> Option<Option<ImageRef>> {
        let mut i = self.lock();
        let found = i
            .derived
            .get(&(Key::of(res, budget), ops))
            .map(|e| e.image.clone());
        if found.is_some() {
            i.stats.derived_hits += 1;
        }
        found
    }

    /// Files a derived image, as [`DecodedImages::insert`]: the first
    /// filed wins.
    pub(crate) fn insert_derived(
        &self,
        res: &BitmapResource,
        budget: &Arc<PixelBudget>,
        ops: [u8; 32],
        image: Option<ImageRef>,
    ) -> Option<ImageRef> {
        let mut i = self.lock();
        i.stats.derived += 1;
        let e = i
            .derived
            .entry((Key::of(res, budget), ops))
            .or_insert_with(|| Entry {
                _pixels: Arc::clone(&res.pixels),
                _original: res.original.clone(),
                _budget: Arc::clone(budget),
                image,
            });
        e.image.clone()
    }

    /// Drops every derived image whose (master, chain) pair is not among
    /// `live`.
    pub(crate) fn retain_derived<'a>(
        &self,
        live: impl Iterator<Item = (&'a BitmapResource, [u8; 32])>,
    ) {
        let live: HashSet<(ResourceId, [u8; 32])> =
            live.map(|(res, ops)| (resource_of(res), ops)).collect();
        let mut i = self.lock();
        let before = i.derived.len();
        i.derived
            .retain(|(k, ops), _| live.contains(&(k.resource(), *ops)));
        let gone = (before - i.derived.len()) as u64;
        i.stats.derived_pruned += gone;
    }

    /// How many derived images are held (failures included).
    #[must_use]
    pub fn derived_len(&self) -> usize {
        self.lock().derived.len()
    }

    /// Drops every entry whose resource is not among `live`.
    pub(crate) fn retain<'a>(&self, live: impl Iterator<Item = &'a BitmapResource>) {
        let live: HashSet<(usize, usize, u32, u32)> = live.map(resource_of).collect();
        let mut i = self.lock();
        let before = i.entries.len();
        i.entries.retain(|k, _| live.contains(&k.resource()));
        let gone = (before - i.entries.len()) as u64;
        i.stats.pruned += gone;
        let before = i.derived.len();
        i.derived.retain(|(k, _), _| live.contains(&k.resource()));
        let gone = (before - i.derived.len()) as u64;
        i.stats.derived_pruned += gone;
    }
}

/// A resource's identity: the addresses of its pixels and original, and
/// its declared size.
pub(crate) type ResourceId = (usize, usize, u32, u32);

/// A resource's identity as [`Key::resource`] spells it.
pub(crate) fn resource_of(res: &BitmapResource) -> ResourceId {
    (
        Arc::as_ptr(&res.pixels) as usize,
        res.original.as_ref().map_or(0, |o| Arc::as_ptr(o) as usize),
        res.info.width,
        res.info.height,
    )
}
