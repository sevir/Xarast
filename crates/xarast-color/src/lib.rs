//! Colour models, conversions and palettes.
//!
//! Covers the colour spaces the `.xar` format and the native format both
//! need — RGB, CMYK, HSV, greyscale and CIE XYZ — the built-in colour table
//! that negative references point at, named document colours, and the
//! derived-colour graph of tints, shades and links.
//!
//! # Two things that look like bugs and are not
//!
//! **CMYK to RGB is naive**, `R = 1 - min(1, C + K)`, exactly as the original
//! does it. The format carries no ICC profiles, and matching Xara's
//! appearance for two decades of existing documents is worth more than being
//! colourimetrically right. See [`ColourValue::to_rgbt`].
//!
//! **The `FIXED24` value `0xF800_0000` is not `-8.0`**, it means "inherit
//! this component from the parent colour". [`Fixed24::to_f32`] returns
//! `Option<f32>` and [`ColourDef::components`] is `[Option<f32>; 4]` so that
//! the check cannot be forgotten.
//!
//! See `docs/phases/phase-01-geometry-and-colour.md` for the specification.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![doc(html_no_source)]

pub mod builtin;
pub mod context;
pub mod fixed24;
pub mod interp;
pub mod model;
pub mod table;

pub use builtin::BuiltinColour;
pub use context::ColourContext;
pub use fixed24::Fixed24;
pub use interp::{FillEffect, RENDER_GREY_WEIGHTS, Stop, TranspMode, Transparency, interpolate};
pub use model::{ColourModel, ColourValue, GREY_MODEL_WEIGHTS, Rgba8, pack_component};
pub use table::{
    Colour, ColourDef, ColourEditError, ColourError, ColourId, ColourIds, ColourKind, ColourTable,
    OnDelete, PaletteEpoch,
};
