//! Colour fidelity across formats (phase 11 W11.5).
//!
//! # What every output says about colour (T11.5.1)
//!
//! Every file Xarast exports is sRGB, and says so where the format has a
//! way to:
//!
//! | Format | Marker |
//! |---|---|
//! | PNG | an `sRGB` chunk, perceptual intent (`png.rs`); an `iCCP` chunk instead when an image's own profile passes through (SVG re-encodes) |
//! | JPEG | APP1 EXIF with `ColorSpace = 1` (`jpeg::exif_srgb`) |
//! | WebP | none: a WebP without an `ICCP` chunk *is* sRGB, and a profile would only restate that at ~3 KiB a file |
//! | PDF | `DeviceRGB`, which PDF viewers treat as sRGB; an sRGB output intent is XARA-T-0230 |
//! | SVG | CSS colours, sRGB by definition; masks carry `color-interpolation="sRGB"` |
//!
//! # What no output carries (T11.5.2, T11.5.3)
//!
//! The document can hold two kinds of colour information none of the five
//! formats is given today, and [`document_compromises`] reports both:
//!
//! - **CMYK and spot colours.** Xarast has no output profile, so a CMYK
//!   colour is written as the naive conversion every renderer of the
//!   document uses (`xarast_color`, `R = 1 − min(1, C + K)`), and a spot
//!   ink as its screen colour. Raster formats can hold nothing else;
//!   SVG's `icc-color()` and PDF's `ICCBased`/`DeviceN` need a profile
//!   (research/06 §6.12.3), which is where they come in later.
//!   → [`Compromise::ColourConverted`].
//! - **Embedded ICC profiles of images.** The renderer does not colour
//!   manage yet (phase 15 converts in `xarast_image::to_working_space`), so
//!   raster and PDF output draw such images as if they were sRGB. SVG does
//!   not lose them: an original PNG, JPEG or GIF passes through byte for
//!   byte, profile included, and a re-encoded bitmap carries its profile
//!   in an `iCCP` chunk ([`crate::png::with_icc_profile`]).
//!   → [`Compromise::ProfileDropped`] for raster and PDF.

use std::sync::Arc;

use xarast_color::{Colour, ColourId, ColourKind, ColourModel, ColourTable};
use xarast_doc::palette::for_each_colour;
use xarast_doc::resources::ImageFormat;
use xarast_doc::{AttrValue, BitmapResource, Document, NodeKind};
use xarast_image::ColourSpace;

use crate::report::Compromise;
use crate::source::ExportSource;

/// Which kind of output a report is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// PNG, JPEG, WebP: the render's pixels.
    Raster,
    /// PDF: vector, images placed as the renderer decoded them.
    Pdf,
    /// SVG: the model, images passed through or re-encoded with their
    /// profile.
    Svg,
}

/// How often the document's colours need a model the output lacks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ColourCensus {
    /// Colour attributes (fill and line colours) whose colour is CMYK.
    pub cmyk: usize,
    /// Colour attributes whose colour is, or derives from, a spot ink.
    pub spot: usize,
    /// Bitmaps with an embedded ICC profile.
    pub icc_images: usize,
}

/// Counts the colour information of `doc` that no export format carries.
///
/// One attribute counts once per kind even when several of its colours
/// (a gradient's stops) are CMYK: the report is about objects, not stops.
#[must_use]
pub fn census(doc: &Document) -> ColourCensus {
    let table = &doc.resources.colours;
    let mut c = ColourCensus::default();
    for n in doc.tree.preorder(doc.tree.root()) {
        let Some(NodeKind::Attr(a)) = doc.tree.kind(n) else {
            continue;
        };
        let (AttrValue::Fill(p) | AttrValue::StrokeColour(p)) = &a.value else {
            continue;
        };
        let (mut cmyk, mut spot) = (false, false);
        for_each_colour(p, |col| {
            cmyk |= is_cmyk(col, table);
            spot |= is_spot(col, table);
        });
        c.cmyk += usize::from(cmyk);
        c.spot += usize::from(spot);
    }
    c.icc_images = doc
        .resources
        .bitmaps()
        .filter(|(_, r)| has_icc_profile(r))
        .count();
    c
}

fn is_cmyk(c: &Colour, table: &ColourTable) -> bool {
    match c {
        Colour::Direct(v) => v.model() == ColourModel::Cmyk,
        Colour::Indexed { id, .. } => table.get(*id).is_some_and(|d| d.model == ColourModel::Cmyk),
        Colour::None => false,
    }
}

/// A spot ink, or a tint, shade or link of one: all print on its plate.
fn is_spot(c: &Colour, table: &ColourTable) -> bool {
    let Colour::Indexed { id, .. } = c else {
        return false;
    };
    let mut at: Option<ColourId> = Some(*id);
    // Palettes are acyclic after the build (`repair_cycles`); the bound
    // only keeps a corrupt table from looping.
    for _ in 0..64 {
        let Some(d) = at.and_then(|i| table.get(i)) else {
            return false;
        };
        if matches!(d.kind, ColourKind::Spot) {
            return true;
        }
        if !d.kind.is_derived() {
            return false;
        }
        at = d.parent;
    }
    false
}

/// Whether a bitmap carries an embedded ICC profile: a header probe of
/// its original PNG or JPEG bytes (the document's `BitmapInfo` records no
/// colour space). Bitmaps with only pixels have none by construction.
#[must_use]
pub fn has_icc_profile(res: &BitmapResource) -> bool {
    let Some(o) = &res.original else {
        return false;
    };
    if !matches!(o.format, ImageFormat::Png | ImageFormat::Jpeg) {
        return false;
    }
    xarast_image::probe(&o.bytes)
        .is_ok_and(|p| matches!(p.info.colour_space, ColourSpace::Icc { .. }))
}

/// The report entries for what the source's document holds and `target`
/// cannot carry. Empty for a scene-only source, which has no model to
/// ask.
#[must_use]
pub fn document_compromises(src: &dyn ExportSource, target: Target) -> Vec<Compromise> {
    let Some(doc) = src.document() else {
        return Vec::new();
    };
    let c = census(doc);
    let mut out = Vec::new();
    if c.cmyk > 0 {
        out.push(Compromise::ColourConverted {
            model: Arc::from("CMYK"),
            count: c.cmyk,
        });
    }
    if c.spot > 0 {
        out.push(Compromise::ColourConverted {
            model: Arc::from("spot"),
            count: c.spot,
        });
    }
    if c.icc_images > 0 && target != Target::Svg {
        out.push(Compromise::ProfileDropped {
            images: c.icc_images,
        });
    }
    out
}
