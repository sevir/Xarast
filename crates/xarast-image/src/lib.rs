//! Image decoding and bitmap resources.
//!
//! Decoding untrusted images is attack surface: every entry point here reads
//! the header, checks it against [`DecodeLimits`], and only then allocates.
//! A file that trips a guard returns a typed [`DecodeError`] and leaves no
//! partial result behind.
//!
//! - [`probe`] reads the header only.
//! - [`decode`] / [`decode_as`] return upright (EXIF-normalised),
//!   premultiplied RGBA8 pixels.
//! - [`decode_on_worker`] adds a hard wall-clock ceiling.
//! - [`xar::decode_xar_bitmap`] unwraps the legacy `.xar` bitmap records.
//!
//! See `docs/phases/phase-10-bitmaps-and-photo.md` and `docs/memory/image.md`.

mod decode;
mod limits;
mod model;
pub mod orient;
pub mod photo;
pub mod pixels;
pub mod procedural;
pub mod sniff;
pub mod xar;

pub use decode::{
    DecodeWarning, DecodedImage, Probe, decode, decode_as, decode_on_worker, probe, probe_as,
    to_working_space,
};
pub use limits::{DecodeError, DecodeLimits, SizeLimit};
pub use model::{
    BitmapData, BitmapInfo, BitmapResource, ColourSpace, DEFAULT_DPI, ImageFormat, OriginalEncoded,
};
pub use orient::Orientation;
pub use sniff::sniff;
