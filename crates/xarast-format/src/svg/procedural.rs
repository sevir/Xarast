//! Clouds and plasma fills: the tile a fill draws, and its bake into the
//! package (phase 13 H8).
//!
//! SVG has no generator to express them, so a procedural fill is written
//! as its model twin (`<xarast:fill xarast:type="fractal-clouds|noise">`
//! with every parameter, the control points and the generator's name)
//! over a base that draws the very tile Xarast draws: the grey tile as a
//! PNG in `resources/baked/` (a `data:` URI in the interchange dialect),
//! in a `<pattern>` on the fill's parallelogram, through the same contone
//! filter as a contone bitmap fill; a transparency is the same tile in a
//! luminance `<mask>`. The tile is generated, never stored in the model:
//! the reader takes the twin and ignores the bake (`research/06 §5.4`).

use std::collections::HashMap;
use std::io::Write as _;
use std::sync::Arc;

use xarast_color::Stop;
use xarast_doc::fill::FillGeometry;
use xarast_geom::{BiasGain, Point};
use xarast_image::procedural::{FillTile, Procedural};

use super::paint::BitmapRef;
use crate::resource::{ResourceId, ResourceKind, resource_path};

/// The tile a procedural fill draws, `None` for any other fill and for a
/// degenerate parallelogram. The renderer and the writer both use it, so
/// the canvas and the bake agree.
#[must_use]
pub fn fill_tile<S: Stop>(fill: &FillGeometry<S>) -> Option<FillTile> {
    let (origin, axis_x, axis_y, params, clouds) = match fill {
        FillGeometry::Fractal {
            origin,
            axis_x,
            axis_y,
            params,
            ..
        } => (origin, axis_x, axis_y, params, true),
        FillGeometry::Noise {
            origin,
            axis_x,
            axis_y,
            params,
            ..
        } => (origin, axis_x, axis_y, params, false),
        _ => return None,
    };
    Some(FillTile {
        clouds,
        seed: params.seed,
        graininess: f64::from(params.graininess),
        gravity: f64::from(params.gravity),
        squash: f64::from(params.squash),
        dpi: params.dpi,
        tileable: params.tileable,
        u_len: length(*origin, *axis_x),
        v_len: length(*origin, *axis_y),
    })
}

/// The distance between two points in millipoints.
fn length(a: Point, b: Point) -> f64 {
    let (ax, ay) = a.to_f64();
    let (bx, by) = b.to_f64();
    (bx - ax).hypot(by - ay)
}

/// The tile as grey bytes, **top row first** (the row at the fill's
/// second end point), with `profile` applied to every value: what an
/// image placed by the bitmap-fill mapping shows.
#[must_use]
pub fn tile_pixels(p: &Procedural, profile: Option<BiasGain>) -> (u32, Vec<u8>) {
    let dim = p.dim();
    let grey = p.generate();
    let lut: Option<Vec<u8>> = profile.filter(|p| *p != BiasGain::IDENTITY).map(|p| {
        (0..=255u8)
            .map(|v| {
                (p.map(f64::from(v) / 255.0) * 255.0)
                    .round()
                    .clamp(0.0, 255.0) as u8
            })
            .collect()
    });
    let side = dim as usize;
    let mut out = Vec::with_capacity(side * side);
    // Row 0 of a generated tile is at the fill's start corner, the
    // bottom of the placed image.
    for row in grey.chunks_exact(side).rev() {
        match &lut {
            Some(l) => out.extend(
                row.iter()
                    .map(|&g| l.get(usize::from(g)).copied().unwrap_or(g)),
            ),
            None => out.extend_from_slice(row),
        }
    }
    (dim, out)
}

/// An 8-bit greyscale PNG, deterministic for the same pixels.
fn grey_png(dim: u32, pixels: &[u8]) -> Vec<u8> {
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&u32::try_from(data.len()).unwrap_or(u32::MAX).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        let mut crc = flate2::Crc::new();
        crc.update(kind);
        crc.update(data);
        out.extend_from_slice(&crc.sum().to_be_bytes());
    }
    let side = dim as usize;
    let mut raw = Vec::with_capacity((side + 1) * side);
    for row in pixels.chunks_exact(side) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    // Writing into a Vec cannot fail.
    let _ = z.write_all(&raw);
    let idat = z.finish().unwrap_or_default();
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&dim.to_be_bytes());
    ihdr.extend_from_slice(&dim.to_be_bytes());
    ihdr.extend_from_slice(&[8, 0, 0, 0, 0]);
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &idat);
    chunk(&mut out, b"IEND", &[]);
    out
}

/// What a tile is baked by: the generator's input and the profile bits.
type BakeKey = (Procedural, Option<(u64, u64)>);

/// The baked tiles of one serialisation, each generated and encoded once.
#[derive(Debug, Default)]
pub(crate) struct Baker {
    /// Write `data:` URIs instead of package paths.
    pub interchange: bool,
    /// The PNGs to store in `resources/baked/`, in first-use order.
    pub files: Vec<Arc<[u8]>>,
    done: HashMap<BakeKey, BitmapRef>,
}

impl Baker {
    /// A baker for the native (package) or interchange dialect.
    pub(crate) fn new(interchange: bool) -> Baker {
        Baker {
            interchange,
            ..Baker::default()
        }
    }

    /// The reference of `tile`'s PNG, with `profile` applied.
    pub(crate) fn tile(&mut self, tile: &Procedural, profile: Option<BiasGain>) -> BitmapRef {
        let profile = profile.filter(|p| *p != BiasGain::IDENTITY);
        let key = (*tile, profile.map(|p| (p.bias.to_bits(), p.gain.to_bits())));
        if let Some(r) = self.done.get(&key) {
            return r.clone();
        }
        let (dim, pixels) = tile_pixels(tile, profile);
        let png = grey_png(dim, &pixels);
        let href = if self.interchange {
            format!("data:image/png;base64,{}", super::xml::base64(&png))
        } else {
            let path = resource_path(ResourceKind::Baked, ResourceId::of(&png), "png");
            self.files.push(Arc::from(png));
            path
        };
        let r = BitmapRef {
            href,
            width: dim,
            height: dim,
            palette: None,
        };
        self.done.insert(key, r.clone());
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xarast_image::procedural::{PlasmaParams, fixed16};

    #[test]
    fn the_png_is_a_valid_grey_image_and_deterministic() {
        let p = Procedural::Plasma(PlasmaParams {
            seed: 3,
            grain: fixed16(10.0),
        });
        let (dim, px) = tile_pixels(&p, None);
        let a = grey_png(dim, &px);
        let b = grey_png(dim, &px);
        assert_eq!(a, b);
        let h = crate::thumbnail::png_header(&a).expect("a PNG header");
        assert_eq!((h.width, h.height), (256, 256));
        // The rows are flipped: the generator's row 0 is the PNG's last.
        let raw = p.generate();
        assert_eq!(&px[255 * 256..], &raw[..256]);
    }

    #[test]
    fn a_tile_is_baked_once() {
        let p = Procedural::Plasma(PlasmaParams {
            seed: 3,
            grain: fixed16(10.0),
        });
        let mut b = Baker::new(false);
        let r1 = b.tile(&p, None);
        let r2 = b.tile(&p, Some(BiasGain::IDENTITY));
        assert_eq!(r1.href, r2.href);
        assert_eq!(b.files.len(), 1);
        assert!(r1.href.starts_with("resources/baked/b3-"), "{}", r1.href);
        let mut i = Baker::new(true);
        assert!(i.tile(&p, None).href.starts_with("data:image/png;base64,"));
        assert!(i.files.is_empty());
    }
}
