//! Font embedding: what a face's licence allows, and the subsets that go
//! into files — WOFF2 web fonts for `.xarast` and SVG (`research/06 §6.7`
//! rules 2–3), font programs for PDF (phase 11 T11.4.7).
//!
//! One layer for every writer, so the `OS/2.fsType` rule is decided in one
//! place ([`FontDb::embed_rights`]):
//!
//! | `fsType` | [`Embedding`] | Embedded? |
//! |---|---|---|
//! | 0 | `Installable` | yes |
//! | bit 3 (0x0008) | `Editable` | yes |
//! | bit 2 (0x0004) | `PreviewPrint` | yes: Xarast only *draws* an embedded subset, it never installs it for editing |
//! | bit 1 alone (0x0002) | `Restricted` | **no** |
//! | bit 9 (0x0200) | `BitmapOnly` | **no** (we embed outlines) |
//! | bit 8 (0x0100) | subsetting forbidden | the whole face's glyph set, never a subset |
//!
//! When several of bits 1–3 are set the least restrictive applies (the
//! OpenType specification's rule). A face whose `OS/2` cannot be read is
//! installable, the specification's default.
//!
//! Subsetting is `subsetter`'s (Typst), which keeps the outlines, metrics
//! and names and drops `cmap` and `OS/2`: right for PDF, where the CID font
//! brings its own maps. A web font needs both, so [`FontDb::web_font`] adds
//! a `cmap` for exactly the characters drawn and the face's own `OS/2`,
//! then wraps the result in WOFF2 ([`woff2`]).

mod cmap;
pub(crate) mod sfnt;
pub mod woff2;
mod woff2_glyf;

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use skrifa::instance::{LocationRef, Size};
use skrifa::string::StringId;
use skrifa::{GlyphId, MetadataProvider};

use crate::font::{FaceData, FaceId, FontDb};
use sfnt::{FLAVOR_CFF, Sfnt};

/// The embedding level `OS/2.fsType` grants.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum Embedding {
    /// No restriction.
    Installable,
    /// May be embedded in documents that are edited.
    Editable,
    /// May be embedded to view and print a document.
    PreviewPrint,
    /// Must not be embedded ("restricted licence").
    Restricted,
    /// Only bitmaps may be embedded; outlines must not.
    BitmapOnly,
}

impl Embedding {
    /// Whether outlines of the face may go into a file.
    #[must_use]
    pub fn allowed(self) -> bool {
        !matches!(self, Embedding::Restricted | Embedding::BitmapOnly)
    }

    /// Why the face is not embedded, for reports; `None` when it may be.
    #[must_use]
    pub fn denial(self) -> Option<&'static str> {
        match self {
            Embedding::Restricted => Some("its licence (OS/2 fsType) forbids embedding"),
            Embedding::BitmapOnly => Some("its licence (OS/2 fsType) allows only bitmap embedding"),
            _ => None,
        }
    }
}

/// What a face's licence allows.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct EmbedRights {
    /// The embedding level.
    pub level: Embedding,
    /// Whether a subset may be embedded; `false` means the whole glyph set.
    pub subsetting: bool,
}

impl EmbedRights {
    /// The rights `fsType` grants (see the module table).
    #[must_use]
    pub fn from_fs_type(fs: u16) -> EmbedRights {
        let level = if fs & 0x0200 != 0 {
            Embedding::BitmapOnly
        } else if fs & 0x0008 != 0 {
            Embedding::Editable
        } else if fs & 0x0004 != 0 {
            Embedding::PreviewPrint
        } else if fs & 0x0002 != 0 {
            Embedding::Restricted
        } else {
            Embedding::Installable
        };
        EmbedRights {
            level,
            subsetting: fs & 0x0100 == 0,
        }
    }
}

/// Why a face could not be embedded.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum EmbedError {
    /// The licence forbids it.
    Denied(Embedding),
    /// The face's bytes are not available or not readable.
    Unreadable,
    /// The subsetter failed on this face.
    Subset(String),
    /// Nothing to embed: none of the requested glyphs or characters exist.
    Empty,
    /// A variable face drawn at a non-default instance: the subsetter keeps
    /// only the default outlines, which would not be what was drawn.
    VariableInstance,
}

impl fmt::Display for EmbedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EmbedError::Denied(e) => f.write_str(e.denial().unwrap_or("its licence forbids it")),
            EmbedError::Unreadable => f.write_str("the font data could not be read"),
            EmbedError::Subset(e) => write!(f, "subsetting failed: {e}"),
            EmbedError::Empty => f.write_str("no glyph to embed"),
            EmbedError::VariableInstance => {
                f.write_str("a variable font instance other than the default")
            }
        }
    }
}

impl std::error::Error for EmbedError {}

/// Outline format of a PDF font program.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum ProgramFormat {
    /// A TrueType font file: `CIDFontType2`, `FontFile2`.
    TrueType,
    /// A bare CID-keyed CFF table: `CIDFontType0`, `FontFile3` with
    /// subtype `CIDFontType0C`.
    Cff,
}

/// A subset font program for a PDF `CIDFont`, with what its dictionaries
/// need. Glyphs are addressed by their **new** glyph id, which is also the
/// CID (identity mapping for both formats).
#[derive(Clone, PartialEq, Debug)]
pub struct PdfFont {
    /// The outline format.
    pub format: ProgramFormat,
    /// The font program (a whole TrueType file, or the CFF table alone).
    pub program: Vec<u8>,
    /// Old glyph id → new glyph id, for every glyph kept.
    pub glyphs: BTreeMap<u32, u16>,
    /// Advance widths per new glyph id, in thousandths of an em.
    pub widths: Vec<f32>,
    /// The PostScript name (`name` ID 6), sanitised for a PDF name; the
    /// family when the face has none.
    pub postscript_name: String,
    /// Font bounding box, thousandths of an em: x0, y0, x1, y1.
    pub bbox: [f32; 4],
    /// Italic angle in degrees (counter-clockwise from vertical).
    pub italic_angle: f32,
    /// Ascent, thousandths of an em.
    pub ascent: f32,
    /// Descent, thousandths of an em (negative).
    pub descent: f32,
    /// Cap height, thousandths of an em.
    pub cap_height: f32,
    /// Fixed pitch.
    pub monospace: bool,
    /// Whether the face is italic or oblique.
    pub italic: bool,
    /// Whether the whole glyph set was kept because the licence forbids
    /// subsetting.
    pub whole: bool,
}

/// A WOFF2 web font for the characters a document draws.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct WebFont {
    /// The WOFF2 file.
    pub woff2: Arc<[u8]>,
    /// How many of the requested characters it maps.
    pub chars: usize,
    /// Whether the whole glyph set was kept because the licence forbids
    /// subsetting.
    pub whole: bool,
}

fn thousandths(v: f32, upem: u16) -> f32 {
    v * 1000.0 / f32::from(upem.max(1))
}

/// Runs the subsetter, turning a panic inside it into an error: the
/// subsetter parses fonts from the system, which are not ours to trust.
fn subset(data: &FaceData, glyphs: &[u16]) -> Result<Vec<u8>, EmbedError> {
    let remapper = subsetter::GlyphRemapper::new_from_glyphs_sorted(glyphs);
    let bytes = data.bytes();
    let index = data.index();
    catch_subsetter(|| subsetter::subset(bytes, index, &remapper))?
        .map_err(|e| EmbedError::Subset(e.to_string()))
}

/// Runs `f` and turns a panic in it into an error. The panic is marked
/// as expected, so the application's hook writes no crash report for it.
fn catch_subsetter<T>(f: impl FnOnce() -> T) -> Result<T, EmbedError> {
    xarast_unwind::expect_panics(f).map_err(|_| EmbedError::Subset("the subsetter panicked".into()))
}

/// A PDF name token: printable ASCII without delimiters.
fn pdf_name(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_graphic() && !"()<>[]{}/%#".contains(*c))
        .collect()
}

impl FontDb {
    /// What the face's `OS/2.fsType` allows. A face without a readable
    /// `OS/2` is installable.
    #[must_use]
    pub fn embed_rights(&self, id: FaceId) -> EmbedRights {
        let fs = self
            .face_data(id)
            .and_then(|d| {
                use skrifa::raw::TableProvider;
                d.font_ref().and_then(|f| f.os2().ok().map(|o| o.fs_type()))
            })
            .unwrap_or(0);
        EmbedRights::from_fs_type(fs)
    }

    /// The glyphs to keep: `wanted` (sorted, deduplicated, `.notdef`
    /// included), or every glyph when the licence forbids subsetting.
    fn keep(
        &self,
        id: FaceId,
        wanted: impl IntoIterator<Item = u32>,
    ) -> Result<(FaceData, Vec<u16>, bool), EmbedError> {
        let rights = self.embed_rights(id);
        if !rights.level.allowed() {
            return Err(EmbedError::Denied(rights.level));
        }
        let data = self.face_data(id).ok_or(EmbedError::Unreadable)?;
        let count = data
            .font_ref()
            .ok_or(EmbedError::Unreadable)?
            .metrics(Size::unscaled(), LocationRef::default())
            .glyph_count;
        let mut glyphs: Vec<u16> = if rights.subsetting {
            wanted
                .into_iter()
                .filter_map(|g| u16::try_from(g).ok())
                .filter(|g| *g < count)
                .collect()
        } else {
            (0..count).collect()
        };
        glyphs.push(0);
        glyphs.sort_unstable();
        glyphs.dedup();
        Ok((data, glyphs, !rights.subsetting))
    }

    /// A subset font program of face `id` holding `glyphs` (glyph ids of
    /// the face), for a PDF `CIDFont`.
    ///
    /// # Errors
    ///
    /// [`EmbedError::Denied`] when the licence forbids embedding; the other
    /// variants when the face cannot be subset.
    pub fn pdf_font(
        &self,
        id: FaceId,
        glyphs: impl IntoIterator<Item = u32>,
    ) -> Result<PdfFont, EmbedError> {
        let (data, keep, whole) = self.keep(id, glyphs)?;
        let font = data.font_ref().ok_or(EmbedError::Unreadable)?;
        let sub = subset(&data, &keep)?;
        let parsed = Sfnt::parse(&sub, 0).ok_or(EmbedError::Unreadable)?;
        let (format, program) = if parsed.flavor == FLAVOR_CFF {
            let cff = parsed.table(b"CFF ").ok_or(EmbedError::Unreadable)?;
            (ProgramFormat::Cff, cff.to_vec())
        } else {
            (ProgramFormat::TrueType, sub)
        };
        let m = font.metrics(Size::unscaled(), LocationRef::default());
        let upem = m.units_per_em;
        let gm = font.glyph_metrics(Size::unscaled(), LocationRef::default());
        let widths = keep
            .iter()
            .map(|g| thousandths(gm.advance_width(GlyphId::from(*g)).unwrap_or(0.0), upem))
            .collect();
        let glyphs = keep
            .iter()
            .enumerate()
            .map(|(new, old)| (u32::from(*old), u16::try_from(new).unwrap_or(u16::MAX)))
            .collect();
        let bbox = m.bounds.map_or([0.0, m.descent, 1000.0, m.ascent], |b| {
            [
                thousandths(b.x_min, upem),
                thousandths(b.y_min, upem),
                thousandths(b.x_max, upem),
                thousandths(b.y_max, upem),
            ]
        });
        let family = self
            .face_info(id)
            .map(|i| i.family.to_string())
            .unwrap_or_default();
        let ps = font
            .localized_strings(StringId::POSTSCRIPT_NAME)
            .english_or_first()
            .map(|s| s.chars().collect::<String>())
            .unwrap_or_default();
        let mut postscript_name = pdf_name(&ps);
        if postscript_name.is_empty() {
            postscript_name = pdf_name(&family);
        }
        if postscript_name.is_empty() {
            postscript_name = "Font".into();
        }
        let italic = self
            .face_info(id)
            .is_some_and(|i| i.style != crate::style::FontStyle::Normal);
        Ok(PdfFont {
            format,
            program,
            glyphs,
            widths,
            postscript_name,
            bbox,
            italic_angle: m.italic_angle,
            ascent: thousandths(m.ascent, upem),
            descent: thousandths(m.descent, upem),
            cap_height: thousandths(m.cap_height.unwrap_or(m.ascent), upem),
            monospace: m.is_monospace,
            italic,
            whole,
        })
    }

    /// A WOFF2 web font of face `id` mapping `chars` (the characters the
    /// document draws with it) to their glyphs.
    ///
    /// # Errors
    ///
    /// [`EmbedError::Denied`] when the licence forbids embedding,
    /// [`EmbedError::Empty`] when the face maps none of `chars`.
    pub fn web_font(&self, id: FaceId, chars: &[char]) -> Result<WebFont, EmbedError> {
        let rights = self.embed_rights(id);
        if !rights.level.allowed() {
            return Err(EmbedError::Denied(rights.level));
        }
        let data = self.face_data(id).ok_or(EmbedError::Unreadable)?;
        let font = data.font_ref().ok_or(EmbedError::Unreadable)?;
        let charmap = font.charmap();
        let mut map: Vec<(u32, u32)> = Vec::new();
        if rights.subsetting {
            let mut cs: Vec<u32> = chars.iter().map(|c| u32::from(*c)).collect();
            cs.sort_unstable();
            cs.dedup();
            for c in cs {
                if let Some(g) = charmap.map(c).filter(|g| g.to_u32() != 0) {
                    map.push((c, g.to_u32()));
                }
            }
        } else {
            map.extend(
                charmap
                    .mappings()
                    .filter(|(_, g)| g.to_u32() != 0)
                    .map(|(c, g)| (c, g.to_u32())),
            );
        }
        if map.is_empty() {
            return Err(EmbedError::Empty);
        }
        let (data, keep, whole) = self.keep(id, map.iter().map(|(_, g)| *g))?;
        let sub = subset(&data, &keep)?;
        let mut s = Sfnt::parse(&sub, 0).ok_or(EmbedError::Unreadable)?;
        let new: BTreeMap<u16, u16> = keep
            .iter()
            .enumerate()
            .map(|(n, o)| (*o, u16::try_from(n).unwrap_or(u16::MAX)))
            .collect();
        let cmap: Vec<(u32, u16)> = map
            .iter()
            .filter_map(|(c, g)| {
                let g = u16::try_from(*g).ok()?;
                Some((*c, *new.get(&g)?))
            })
            .collect();
        s.tables.insert(*b"cmap", cmap::build(&cmap));
        // Browsers require `OS/2`; the face's own, unchanged (its fsType
        // included, so the file still states its licence).
        let original = Sfnt::parse(data.bytes(), data.index()).ok_or(EmbedError::Unreadable)?;
        if let Some(os2) = original.table(b"OS/2") {
            s.tables.insert(*b"OS/2", os2.to_vec());
        }
        let woff2 = woff2::encode(&s).ok_or(EmbedError::Unreadable)?;
        Ok(WebFont {
            woff2: Arc::from(woff2),
            chars: cmap.len(),
            whole,
        })
    }
}

/// A copy of a single-face font file with its `OS/2.fsType` set to `fs`,
/// for fixtures: a face whose licence forbids embedding without shipping
/// one. `None` when the file has no `OS/2` table.
#[must_use]
pub fn with_fs_type(font: &[u8], fs: u16) -> Option<Vec<u8>> {
    let mut s = Sfnt::parse(font, 0)?;
    let os2 = s.tables.get_mut(b"OS/2")?;
    os2.get_mut(8..10)?.copy_from_slice(&fs.to_be_bytes());
    Some(s.to_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_caught_subsetter_panic_is_marked_expected_for_the_hook() {
        use std::sync::{Arc, Mutex};
        // What a hook would see on this thread: the marker at each panic.
        let seen = Arc::new(Mutex::new(Vec::new()));
        let me = std::thread::current().id();
        let previous = std::panic::take_hook();
        let s = Arc::clone(&seen);
        std::panic::set_hook(Box::new(move |_| {
            if std::thread::current().id() == me {
                s.lock().unwrap().push(xarast_unwind::panics_expected());
            }
        }));
        let r = catch_subsetter(|| -> u8 { panic!("a hostile font") });
        drop(std::panic::take_hook());
        std::panic::set_hook(previous);
        assert!(matches!(r, Err(EmbedError::Subset(_))));
        assert_eq!(*seen.lock().unwrap(), [true], "the hook ran once, expected");
        assert!(!xarast_unwind::panics_expected(), "the scope is closed");
    }

    #[test]
    fn fs_type_bits_give_the_documented_rights() {
        let r = EmbedRights::from_fs_type;
        assert_eq!(r(0).level, Embedding::Installable);
        assert_eq!(r(0x0002).level, Embedding::Restricted);
        assert_eq!(r(0x0004).level, Embedding::PreviewPrint);
        assert_eq!(r(0x0008).level, Embedding::Editable);
        // The least restrictive of bits 1-3 wins.
        assert_eq!(r(0x0006).level, Embedding::PreviewPrint);
        assert_eq!(r(0x000A).level, Embedding::Editable);
        assert_eq!(r(0x0200).level, Embedding::BitmapOnly);
        assert!(!r(0x0200).level.allowed());
        assert!(!r(0x0002).level.allowed());
        assert!(r(0x0004).level.allowed());
        assert!(r(0).subsetting);
        assert!(!r(0x0100).subsetting);
    }
}
