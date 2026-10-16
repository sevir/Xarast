//! Placing images (phase 10, T10.3.8): a file dropped on the canvas or a
//! picture pasted from the clipboard becomes a bitmap object at its
//! natural size, centred on the drop point or in the view, as one undo
//! step (`xarast_doc::PlaceBitmap`).
//!
//! The image is decoded once here, under the default decode limits, so a
//! file that cannot be read, is not an image, or trips a guard is refused
//! with a message before anything is added to the document. The resource
//! keeps the file's own bytes when the document can store them as they are
//! (PNG, JPEG, GIF); any other format is converted to a lossless PNG once,
//! here, keeping its resolution. A picture from the clipboard arrives as
//! pixels and is stored as a PNG too. Decoded pixels are not kept in the
//! document: the walker decodes resources itself, as it does for opened
//! files.
//!
//! The resource is added outside the undo history, as a pasted fragment's
//! bitmaps are (`tools.md` decision 41): an undone placement leaves an
//! unreferenced bitmap that the save-time sweep removes.

use std::sync::Arc;

use xarast_doc::bitmap_fill::natural_length;
use xarast_doc::{BitmapData, BitmapId, BitmapInfo, BitmapResource, Document, OriginalEncoded};
use xarast_geom::Mp;
use xarast_image::{DecodeLimits, ImageFormat};

/// Why an image could not be placed.
#[derive(Debug, thiserror::Error)]
pub enum PlaceError {
    /// The file could not be read.
    #[error("could not read {0}")]
    Read(String),
    /// The bytes are not an image Xarast decodes, or a decode guard refused
    /// them.
    #[error("not an image Xarast can place: {0}")]
    Decode(#[from] xarast_image::DecodeError),
    /// Converting it for storage failed.
    #[error("could not store the image: {0}")]
    Encode(String),
    /// A clipboard picture whose buffer does not match its size.
    #[error("the pasted picture is malformed")]
    Malformed,
    /// The user cancelled a background import (T10.7.5).
    #[error("the import was cancelled")]
    Cancelled,
}

/// An image ready to place: the document resource and its natural size.
#[derive(Debug, Clone)]
pub struct ImageToPlace {
    /// The resource, with its encoded bytes and no decoded pixels.
    pub resource: BitmapResource,
    /// Its size in pixels.
    pub pixels: (u32, u32),
    /// Its resolution in dots per inch, per axis.
    pub dpi: (u32, u32),
}

impl ImageToPlace {
    /// The natural size in millipoints: `pixels × 72 000 / dpi` per axis.
    #[must_use]
    pub fn natural_size(&self) -> (Mp, Mp) {
        (
            natural_length(self.pixels.0, self.dpi.0),
            natural_length(self.pixels.1, self.dpi.1),
        )
    }
}

/// Whether a path names a file this module can place, by its extension:
/// what a drop on the canvas imports instead of opening as a document.
#[must_use]
pub fn is_image_path(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .is_some_and(|e| {
            matches!(
                e.as_str(),
                "png"
                    | "jpg"
                    | "jpeg"
                    | "jpe"
                    | "gif"
                    | "webp"
                    | "tif"
                    | "tiff"
                    | "bmp"
                    | "pnm"
                    | "pbm"
                    | "pgm"
                    | "ppm"
                    | "pam"
            )
        })
}

/// The image files a clipboard text names, when the text is a list of
/// files — what a file manager's Copy leaves there (phase 10, T10.7.4):
/// `text/uri-list` (`file:///…`, percent-encoded, `#` comments), GNOME's
/// `x-special/gnome-copied-files` (a first line `copy` or `cut`, then
/// URIs), or plain absolute paths one per line. Every line must name a
/// local file, so ordinary text never reads as a list; of those, only the
/// images ([`is_image_path`]) are returned. Empty when the text is not a
/// file list or lists no image.
#[must_use]
pub fn image_paths_in_text(text: &str) -> Vec<std::path::PathBuf> {
    let mut lines = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .peekable();
    if lines.peek().is_some_and(|l| *l == "copy" || *l == "cut") {
        lines.next();
    }
    let mut out = Vec::new();
    let mut any = false;
    for line in lines {
        any = true;
        let path = if let Some(rest) = line.strip_prefix("file://") {
            // `file://host/path`: an empty or `localhost` authority is ours.
            let path = match rest.find('/') {
                Some(0) => rest,
                Some(i) if &rest[..i] == "localhost" => &rest[i..],
                _ => return Vec::new(),
            };
            percent_decode(path)
        } else if line.starts_with('/') {
            line.to_owned()
        } else {
            return Vec::new();
        };
        let path = std::path::PathBuf::from(path);
        if is_image_path(&path) {
            out.push(path);
        }
    }
    if any { out } else { Vec::new() }
}

/// `%XX` escapes to bytes; anything malformed is kept as it is.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(hi), Some(lo)) = (
                char::from(b[i + 1]).to_digit(16),
                char::from(b[i + 2]).to_digit(16),
            )
        {
            out.push(u8::try_from(hi * 16 + lo).unwrap_or(b'%'));
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn doc_format(f: ImageFormat) -> Option<xarast_doc::ImageFormat> {
    match f {
        ImageFormat::Png => Some(xarast_doc::ImageFormat::Png),
        ImageFormat::Jpeg => Some(xarast_doc::ImageFormat::Jpeg),
        ImageFormat::Gif => Some(xarast_doc::ImageFormat::Gif),
        _ => None,
    }
}

/// Straight RGBA8 as a PNG, with its resolution in `pHYs` when it has one.
pub(crate) fn encode_png(
    width: u32,
    height: u32,
    rgba: &[u8],
    dpi: Option<u32>,
) -> Result<Vec<u8>, String> {
    let header = xarast_io::png::PngHeader {
        width,
        height,
        colour: xarast_io::PngColour::Rgba,
        depth: xarast_io::PngDepth::Eight,
        interlace: false,
        // Pixels per metre, as `pHYs` wants it.
        ppm: dpi.map(|d| (f64::from(d) / 0.0254).round() as u32),
        level: 6,
    };
    let mut out = Vec::new();
    xarast_io::png::encode_png(&mut out, header, rgba).map_err(|e| e.to_string())?;
    Ok(out)
}

fn resource(
    name: &str,
    format: xarast_doc::ImageFormat,
    bytes: Arc<[u8]>,
    pixels: (u32, u32),
    dpi: (u32, u32),
) -> BitmapResource {
    BitmapResource {
        name: Arc::from(name),
        info: BitmapInfo {
            width: pixels.0,
            height: pixels.1,
            bpp: 32,
            dpi_x: dpi.0,
            dpi_y: dpi.1,
        },
        pixels: Arc::new(BitmapData::default()),
        original: Some(Arc::new(OriginalEncoded { format, bytes })),
        procedural: None,
        transparent_index: None,
    }
}

/// An image from a file's bytes: decoded once to check it and to learn its
/// size and resolution.
///
/// # Errors
///
/// [`PlaceError::Decode`] for anything that is not a decodable image, or
/// that a decode guard refuses; [`PlaceError::Encode`] when converting it
/// to PNG fails.
pub fn image_from_bytes(bytes: Arc<[u8]>, name: &str) -> Result<ImageToPlace, PlaceError> {
    let decoded = xarast_image::decode(&bytes, &DecodeLimits::default())?;
    let info = &decoded.info;
    let pixels = (info.pixel_width, info.pixel_height);
    let dpi = (info.hdpi, info.vdpi);
    let (format, stored) = match doc_format(decoded.format) {
        Some(f) => (f, bytes),
        None => {
            let rgba = decoded.data.to_straight_rgba8();
            let png =
                encode_png(pixels.0, pixels.1, &rgba, Some(dpi.0)).map_err(PlaceError::Encode)?;
            (xarast_doc::ImageFormat::Png, Arc::from(png))
        }
    };
    Ok(ImageToPlace {
        resource: resource(name, format, stored, pixels, dpi),
        pixels,
        dpi,
    })
}

/// An image from a file.
///
/// # Errors
///
/// [`PlaceError::Read`] when the file cannot be read, then as
/// [`image_from_bytes`].
pub fn image_from_file(path: &std::path::Path) -> Result<ImageToPlace, PlaceError> {
    let bytes =
        std::fs::read(path).map_err(|e| PlaceError::Read(format!("{}: {e}", path.display())))?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    image_from_bytes(Arc::from(bytes), &name)
}

/// An image from straight RGBA8 pixels — a picture on the clipboard. It
/// has no resolution of its own, so it takes the screen's default
/// ([`xarast_image::DEFAULT_DPI`]), and is stored as a PNG.
///
/// # Errors
///
/// [`PlaceError::Malformed`] when the buffer does not hold `width ×
/// height` pixels or the size is empty; [`PlaceError::Encode`] when the
/// PNG cannot be written.
pub fn image_from_rgba(width: u32, height: u32, rgba: &[u8]) -> Result<ImageToPlace, PlaceError> {
    let len = u64::from(width) * u64::from(height) * 4;
    if width == 0 || height == 0 || len != rgba.len() as u64 {
        return Err(PlaceError::Malformed);
    }
    let dpi = xarast_image::DEFAULT_DPI;
    let png = encode_png(width, height, rgba, None).map_err(PlaceError::Encode)?;
    Ok(ImageToPlace {
        resource: resource(
            "Pasted image",
            xarast_doc::ImageFormat::Png,
            Arc::from(png),
            (width, height),
            (dpi, dpi),
        ),
        pixels: (width, height),
        dpi: (dpi, dpi),
    })
}

/// A bitmap resource's size in pixels and its resolution, from its layout
/// when the document recorded one, else from the header of its encoded
/// bytes (the `.xar` importer leaves the layout empty). `None` when neither
/// says.
#[must_use]
pub fn bitmap_pixels(doc: &Document, id: BitmapId) -> Option<((u32, u32), (u32, u32))> {
    let res = doc.resources.bitmap(id)?;
    let i = res.info;
    if i.width > 0 && i.height > 0 {
        let d = |v: u32| if v == 0 { xarast_image::DEFAULT_DPI } else { v };
        return Some(((i.width, i.height), (d(i.dpi_x), d(i.dpi_y))));
    }
    let bytes = &res.original.as_ref()?.bytes;
    let p = xarast_image::probe(bytes).ok()?;
    Some((
        (p.info.pixel_width, p.info.pixel_height),
        (p.info.hdpi, p.info.vdpi),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_paths_are_recognised_by_extension() {
        for p in ["a.png", "b.JPG", "c.webp", "d.tiff", "e.bmp"] {
            assert!(is_image_path(std::path::Path::new(p)), "{p}");
        }
        for p in ["a.xar", "b.xarast", "c.svg", "noext"] {
            assert!(!is_image_path(std::path::Path::new(p)), "{p}");
        }
    }

    #[test]
    fn a_file_managers_copy_names_its_images() {
        use std::path::PathBuf;
        let uri_list =
            "# a comment\r\nfile:///home/a/My%20Photo.JPG\r\nfile:///home/a/notes.txt\r\n";
        assert_eq!(
            image_paths_in_text(uri_list),
            [PathBuf::from("/home/a/My Photo.JPG")]
        );
        let gnome = "copy\nfile://localhost/tmp/x.png\nfile:///tmp/y.webp";
        assert_eq!(
            image_paths_in_text(gnome),
            [PathBuf::from("/tmp/x.png"), PathBuf::from("/tmp/y.webp")]
        );
        assert_eq!(
            image_paths_in_text("/tmp/a.png\n/tmp/b.gif\n"),
            [PathBuf::from("/tmp/a.png"), PathBuf::from("/tmp/b.gif")]
        );
        // Ordinary text, a remote file, a list with no image, nothing.
        for t in [
            "see /tmp/a.png",
            "a.png",
            "file://server/share/a.png",
            "file:///tmp/a.txt",
            "copy",
            "",
            "<svg/>",
        ] {
            assert!(image_paths_in_text(t).is_empty(), "{t:?}");
        }
    }

    #[test]
    fn a_clipboard_picture_is_stored_as_a_png_at_the_default_resolution() {
        let rgba = [255u8, 0, 0, 255].repeat(96 * 48);
        let img = image_from_rgba(96, 48, &rgba).unwrap();
        assert_eq!(img.natural_size(), (Mp::new(72_000), Mp::new(36_000)));
        let bytes = &img.resource.original.as_ref().unwrap().bytes;
        let back = image_from_bytes(Arc::clone(bytes), "x").unwrap();
        assert_eq!(back.pixels, (96, 48));
        assert!(image_from_rgba(2, 2, &[0; 15]).is_err());
        assert!(image_from_rgba(0, 0, &[]).is_err());
    }

    #[test]
    fn a_resolution_in_the_file_sets_the_natural_size() {
        let rgba = [0u8, 0, 255, 255].repeat(300 * 150);
        let png = encode_png(300, 150, &rgba, Some(300)).unwrap();
        let img = image_from_bytes(Arc::from(png), "hi-res.png").unwrap();
        assert_eq!(img.dpi, (300, 300));
        assert_eq!(img.natural_size(), (Mp::new(72_000), Mp::new(36_000)));
        assert_eq!(
            img.resource.original.as_ref().unwrap().format,
            xarast_doc::ImageFormat::Png
        );
    }

    #[test]
    fn what_is_not_an_image_is_refused() {
        assert!(matches!(
            image_from_bytes(Arc::from(&b"not an image"[..]), "x"),
            Err(PlaceError::Decode(_))
        ));
        assert!(matches!(
            image_from_file(std::path::Path::new("/nonexistent/x.png")),
            Err(PlaceError::Read(_))
        ));
    }
}
