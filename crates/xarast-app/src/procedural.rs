//! Clouds and plasma fills materialised into images (phase 13 H5).
//!
//! A procedural fill reaches the renderer as an ordinary contone
//! [`Paint::Image`](xarast_render::Paint::Image): its grey tile, generated
//! by `xarast_image::procedural`, mapped onto the fill's parallelogram
//! through the fill's two colours or levels. The tile is generated once
//! per **full parameter tuple** — the generator's inputs including the
//! tile size, plus the colour fill's bias/gain profile, which is baked
//! into the tile — and the walker that owns this cache keeps it across
//! frames: a redraw, a pan or an edit of another object never regenerates
//! a tile (the original's `IsSameAsCachedFractal`, `research/03 §2.8.3`).
//!
//! # Lifetime
//!
//! A tile no walk used stays registered while the cache holds at most
//! [`KEEP`] tiles; beyond that, the tiles the last walk did not use are
//! released (their registry slots parked with a 1 × 1 placeholder and
//! reused), so dragging a seed or graininess slider does not pile up
//! 256 KiB images.

use std::collections::{HashMap, HashSet};

use xarast_doc::fill::FillGeometry;
use xarast_format::svg::procedural::TileColours;
use xarast_geom::BiasGain;
use xarast_image::procedural::Procedural;
use xarast_render::{ImageId, ImageRef, Resolver};

/// How many tiles stay registered whether or not the last walk used them.
pub(crate) const KEEP: usize = 128;

/// What a tile is cached by: the generator's whole input and the profile
/// baked into it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct TileKey {
    tile: Procedural,
    /// The bias/gain profile's bits; `None` for the identity (and for a
    /// transparency, whose profile shapes its level ramp instead).
    profile: Option<(u64, u64)>,
    /// A colour fill's colours, baked into the tile; `None` for a
    /// transparency, whose tile stays grey.
    colours: Option<TileColours>,
}

impl TileKey {
    /// The key of a procedural fill, `None` when it is not one or its
    /// parallelogram is degenerate (the original does not generate it).
    /// The tile is the one the `.xarast` writer bakes
    /// ([`xarast_format::svg::procedural::fill_tile`]).
    pub(crate) fn of<S: xarast_color::Stop>(
        fill: &FillGeometry<S>,
        colours: Option<TileColours>,
    ) -> Option<TileKey> {
        let with_profile = colours.is_some();
        let tile = xarast_format::svg::procedural::fill_tile(fill)?.procedural()?;
        let profile = match fill {
            FillGeometry::Fractal { profile, .. } | FillGeometry::Noise { profile, .. }
                if with_profile && *profile != BiasGain::IDENTITY =>
            {
                Some((profile.bias.to_bits(), profile.gain.to_bits()))
            }
            _ => None,
        };
        Some(TileKey {
            tile,
            profile,
            colours,
        })
    }

    /// The tile as straight RGBA, top row first, the profile applied:
    /// the colours' table, or grey.
    fn rgba(&self) -> (u32, Vec<u8>) {
        let profile = self
            .profile
            .map(|(b, g)| BiasGain::new(f64::from_bits(b), f64::from_bits(g)));
        if let Some(c) = self.colours {
            return xarast_format::svg::procedural::tile_rgba(&self.tile, profile, c);
        }
        let (dim, grey) = xarast_format::svg::procedural::tile_pixels(&self.tile, profile);
        let mut out = Vec::with_capacity(grey.len() * 4);
        for g in grey {
            out.extend_from_slice(&[g, g, g, 255]);
        }
        (dim, out)
    }
}

/// The tiles a walker has registered.
#[derive(Debug, Default)]
pub(crate) struct ProceduralImages {
    tiles: HashMap<TileKey, ImageId>,
    used: HashSet<TileKey>,
    free: Vec<ImageId>,
    placeholder: Option<ImageRef>,
    generated: u64,
}

impl ProceduralImages {
    /// The registered image of `key`, generated on first use.
    pub(crate) fn image(&mut self, resolver: &mut Resolver, key: TileKey) -> ImageId {
        self.used.insert(key);
        if let Some(id) = self.tiles.get(&key) {
            return *id;
        }
        let (dim, rgba) = key.rgba();
        let image = ImageRef::new(dim, dim, rgba);
        let id = match self.free.pop() {
            Some(slot) => {
                resolver.images.replace(slot, image);
                slot
            }
            None => resolver.images.insert(image),
        };
        self.generated += 1;
        self.tiles.insert(key, id);
        id
    }

    /// Ends a walk: beyond [`KEEP`] tiles, releases those it did not use.
    pub(crate) fn end_walk(&mut self, resolver: &mut Resolver) {
        if self.tiles.len() > KEEP {
            let dead: Vec<TileKey> = self
                .tiles
                .keys()
                .filter(|k| !self.used.contains(*k))
                .copied()
                .collect();
            if !dead.is_empty() {
                let placeholder = self
                    .placeholder
                    .get_or_insert_with(|| ImageRef::new(1, 1, vec![0; 4]))
                    .clone();
                for k in dead {
                    if let Some(slot) = self.tiles.remove(&k) {
                        resolver.images.replace(slot, placeholder.clone());
                        self.free.push(slot);
                    }
                }
            }
        }
        self.used.clear();
    }

    /// Forgets everything (the resolver it registered with is gone).
    pub(crate) fn clear(&mut self) {
        *self = ProceduralImages::default();
    }

    /// How many tiles this cache has generated in its life.
    pub(crate) fn generated(&self) -> u64 {
        self.generated
    }
}
