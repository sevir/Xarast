# The `.xar` file format (CXF / Camelot eXchange Format) — technical specification

> **Clean-room notice.** This document describes the *behaviour* and the *data
> formats* of Xara Xtreme (GPL-2.0-only) for interoperability purposes: binary
> layout of the `.xar` container, tag numbering and the semantics of each
> record. It does not reproduce source code from the original; the
> `file:line` references point at the reference tree in `xara-xtreme/` and
> serve only to locate the logic being described. Xarast is implemented from
> this specification, not by translating the original.

> **Status:** normative reference document for implementing the `.xar` importer in Rust.
> **Origin:** reverse engineering of the original **Xara Xtreme / Xara LX** source code
> (`/home/user/xara-xtreme`, C++, GPL v2), validated against 59 real `.xar` files from
> that same repository (`testfiles/`, `Designs/`, `Templates/`, `TextDesigns/`).
> All cross-references take the form `file:line` relative to the root of the
> original tree (for example `Kernel/cxfile.cpp:1699`).
>
> **Nomenclature:** the format is internally called the *v2 file format* or *CXF*. The
> `.xar` extension corresponds to the native format (`CXN`) and `.web` to the web format
> (`CXW`/`CXM`); both share **exactly** the same binary structure (see §1.4).

---

## Contents

1. [Physical structure of the file](#1-physical-structure-of-the-file)
2. [Record structure](#2-record-structure)
3. [Compression](#3-compression)
4. [Complete table of tags and layouts](#4-complete-table-of-tags-and-layouts)
5. [Coordinate system and units](#5-coordinate-system-and-units)
6. [Tree model, definitions and references](#6-tree-model-definitions-and-references)
7. [Path representation](#7-path-representation)
8. [Attributes and their inheritance model](#8-attributes-and-their-inheritance-model)
9. [Colours](#9-colours)
10. [Implementation priorities](#10-implementation-priorities)
11. [Known risks and ambiguities](#11-known-risks-and-ambiguities)
12. [Appendices](#12-appendices)

---

## 1. Physical structure of the file

### 1.1. Overview

A `.xar` file is:

```
+-------------------------------------------------------------+
| 8 bytes of signature (magic)                                 |
+-------------------------------------------------------------+
| Sequence of records, each one: tag u32, size u32, data       |
|   ...                                                        |
|   TAG_STARTCOMPRESSION  -> from here on, a deflate stream    |
|   ...compressed records...                                   |
|   TAG_ENDCOMPRESSION    -> followed by 8 bytes CRC+size      |
|   ...                                                        |
|   TAG_ENDOFFILE                                              |
+-------------------------------------------------------------+
```

There is no table of contents, no index and no absolute offsets: **the file is a
sequential stream of records**. The only "pointer" in the format is the *record number*
(the ordinal position within the file), used to reference definitions (§6.3).

### 1.2. Signature / magic bytes

Definition: `Kernel/cxfdefs.h:109-110`

| Constant | Value (`u32`) | Defined in |
|---|---|---|
| `CXF_IDWORD1` | `0x41524158` | `Kernel/cxfdefs.h:109` |
| `CXF_IDWORD2` | `0x0A0DA3A3` | `Kernel/cxfdefs.h:110` |

They are written as two **little-endian** `u32` values (`Kernel/cxfile.cpp:241-242`), so
that the first 8 bytes of the file are exactly:

```
offset 0: 58 41 52 41 A3 A3 0D 0A        ("XARA" + A3 A3 CR LF)
```

Validated across all 59 files in the corpus. Format detection (`HowCompatible`,
`Kernel/camfiltr.cpp:1639-1690`) requires these 8 bytes; it optionally checks that the
next record is `TAG_FILEHEADER` (2) with `size > 3` and reads the first 3 bytes of its
payload as a type identifier (`CXN`/`CXW`/`CXM`).

The sequence `A3 A3 0D 0A` is the classic anti-ASCII-transfer trap (if an FTP client in
text mode converts CRLF↔LF, the signature stops validating).

### 1.3. Byte order (endianness)

**The entire format is little-endian**, without exception, in every multi-byte type:

* `Kernel/cxfile.cpp:875-905` — `CXaraFile::Read(UINT32*)` applies `LEtoNative()`.
* `Kernel/cxfrec.cpp:597-676` — `WriteUINT32/INT32/UINT16/INT16/FLOAT/DOUBLE/FIXED16/ANGLE`
  apply `NativetoLE()`.
* `Kernel/cxfrec.cpp:678-686` — `WriteWCHAR` writes UTF-16 **LE**.

The one "odd ordering" exception: the *interleaved* coordinates of relative paths, which
write the bytes of X and Y alternately from most significant to least significant (§7.3).
This is not big-endian in the file: it is a deliberate permutation within an 8-byte field,
done to improve zlib compression (`Kernel/cxfrec.cpp:752-790`).

### 1.4. Logical header: `TAG_FILEHEADER` (tag 2)

Writing: `Kernel/camfiltr.cpp:3749-3768` · Reading: `Kernel/cxfile.cpp:2520-2547`

It is the **first record** in the file, always **uncompressed**.

| Offset | Type | Field | Notes |
|---|---|---|---|
| 0 | `[u8;3]` | `file_type` | ASCII, **with no terminator**: `"CXN"`, `"CXW"` or `"CXM"` |
| 3 | `u32` | `file_size` | Total **uncompressed** size of the file, in bytes (used for the progress bar). It is written as 0 and patched at the end of the export: `Kernel/camfiltr.cpp:4115-4125` |
| 7 | `u32` | `native_web_link_id` | Link identifier between the native file and its web version. Always 0 in the corpus |
| 11 | `u32` | `precompression_flags` | "Precompression" flags. **Must be 0**; any other value makes the importer abort with `IDS_UNKNOWN_COMPRESSION` (`Kernel/camfiltr.cpp:832-845`) |
| 15 | ASCII-Z | `producer` | e.g. `"Xara X"` |
| … | ASCII-Z | `producer_version` | e.g. `"3.0"` |
| … | ASCII-Z | `producer_build` | e.g. `"0.2704 (MarkG)"` |

The three strings are ASCII terminated by `NUL` (§2.4). The record size is variable
(36–47 bytes in the corpus).

A real example (`testfiles/OneLine.xar`, record #1, size 41):

```
43 58 4E | 9C 0F 00 00 | 00 00 00 00 | 00 00 00 00 |
"Xara X\0" "3.0\0" "0.2704 (MarkG)\0"
 -> type="CXN", file_size=3996, link=0, precomp=0
```

### 1.5. Versions

The format **has no global version number**. The "version" is deduced from three things:

1. **The tag range.** `Kernel/cxftags.h:109-114` documents the assigned ranges:
   `0–3999` = Camelot v1.5 (the first version of the format, **frozen**),
   `4000–4999` = Camelot v2.0 and later (Xara X, Xara X1/X2, XaraLX).
2. **The `TAG_FILEHEADER` strings** (`producer`, `producer_version`, `producer_build`):
   this is the only "program version" stored, and it is purely informational.
3. **The actual size of each record.** Several records grew between versions (e.g.
   `TAG_LINEARFILL` went from 24 to 40 bytes when the bias/gain profile was added, see
   `Kernel/cxfdefs.h:253`; `TAG_TEXT_STORY_SIMPLE` from 8 to 12 when the autokern flag was
   added, `Kernel/cxfdefs.h:470`). **The reader must tolerate records shorter than
   expected and fill in default values**: the original code does exactly that by means of
   "noError" reads (`ReadINT32noError`, `ReadDOUBLEnoError`,
   `Kernel/cxfrec.cpp:1290,1342`; used in `Kernel/rechtext.cpp:756-770`).

There is also a version **of the compression subsystem** inside
`TAG_STARTCOMPRESSION` (§3.2).

### 1.6. Differences between `.xar` (native) and `.web`

Identifiers (`Kernel/camfiltr.h:143-145`):

| Constant in the original | Value (3 ASCII characters) | Format it identifies |
|---|---|---|
| `EXPORT_FILETYPE_WEB` | `CXW` | web format (`.web`) |
| `EXPORT_FILETYPE_MIN` | `CXM` | "minimal" web format |
| `EXPORT_FILETYPE_NATIVE` | `CXN` | native format (`.xar`) |

This identifier is the `type` field of `TAG_FILEHEADER` (§1.4).

* **The binary structure is identical.** The same reader serves all three.
* The difference is *which* records are emitted. The exporter calls
  `WritePreChildrenWeb()` or `WritePreChildrenNative()` depending on the filter
  (`Kernel/camfiltr.cpp:4532-4545`). In the vast majority of classes,
  `WritePreChildrenNative()` simply delegates to the Web version
  (e.g. `Kernel/fillattr.cpp:5981-5987`), so the content matches.
* What appears in native **only**: document information that does not affect rendering
  (e.g. `TAG_DOCUMENTFLAGS`, `Kernel/infocomp.cpp:736-738` checks `IsWebFilter()` and does
  not write it in web), undo size, comments, views, and so on.
* The `CXM` format ("minimal web") indicates that the document needs a default template
  when it is loaded (`Kernel/webfiltr.cpp:383-390`).
* There is also a **text format** (`CXaraTemplateFile`, `Kernel/cxftfile.h:106-140`) used
  for Flare templates: it translates each record into text and the binary data into
  BinHex. **Outside the scope of this document.**

**Recommendation for Rust:** accept `CXN`, `CXW` and `CXM` with the same parser and expose
the detected type in the API.

---

## 2. Record structure

### 2.1. Binary layout

Writing: `Kernel/cxfile.cpp:1699-1717` and `Kernel/cxfile.cpp:1636-1657`
Reading: `Kernel/cxfile.cpp:1855-1866`

```
+--------+--------+-------------------------------+
| tag:u32| size:u32|  payload: size bytes          |
+--------+--------+-------------------------------+
```

* `tag`: little-endian `u32`. Identifies the type of the record.
* `size`: little-endian `u32`. **Size of the payload in bytes, not counting the 8 header
  bytes.** It may be 0.
* `payload`: exactly `size` bytes.

**There is no alignment and no padding of any kind.** Records follow one another with no
gaps, and the fields within the payload are not aligned either (e.g.
`TAG_SPREADINFORMATION` has a `u8` at the end of four `i32`s;
`TAG_DEFINECOMPLEXCOLOUR` starts with 5 loose bytes followed by a `u32`). **A Rust
`#[repr(C)]` is NO use for mapping payloads**: they have to be read field by field (or use
`#[repr(C, packed)]` with great care and explicit endianness conversion).

The value `CXF_UNKNOWN_SIZE = -1` (`Kernel/cxfdefs.h:111`) **never appears in the file**:
it is an internal marker in the writer meaning "the size is computed when the record is
closed". In the file the size is always the real one.

### 2.2. Variable-size records

Three mechanisms produce a variable size:

1. **Embedded strings** (layer, colour, font names, URLs…): NUL-terminated, and the record
   size includes them.
2. **Arrays with an explicit count**: e.g. the colour ramp of multi-stage gradients
   (`u32 n` followed by `n` pairs), `Kernel/fillattr.cpp:6837-6843`.
3. **Arrays *without* a count, deduced from the record size**: the most important case is
   that of relative paths, where `num_coords = size / 9`
   (`Kernel/cxfrec.cpp:1837`). Also `TAG_ATOMICTAGS` (`size/4` tags,
   `Kernel/cxfile.cpp:2574`) and `TAG_PATH_FLAGS` (`size` = number of points).

### 2.3. Unknown records: how they are ignored

`Kernel/cxfile.cpp:1997-2065` (`CXaraFile::ReadNextRecord`):

```text
if no handler is registered for the tag that was read:
    consume the `size` bytes of the payload without interpreting them
    carry on with the next record
```

That is: **skip `size` bytes and carry on**. But there are two critical nuances
(`Kernel/camfiltr.cpp:5293-5312`, `BaseCamelotFilter::UnrecognisedTag`):

* If the tag is in the **essential** list (`TAG_ESSENTIALTAGS`, tag 11) → **abort the
  import**: the file cannot be represented faithfully.
* If the tag is in the **atomic** list (`TAG_ATOMICTAGS`, tag 10) → its **entire subtree
  must be discarded too** (`StripNextSubTree()`, `Kernel/cxfile.cpp:620-632`), that is,
  ignore every record up to the `TAG_UP` that closes the `TAG_DOWN` following the unknown
  record. Reason: if you do not understand a composite node (bevel, contour, shadow,
  ClipView, live effect), its children are *derived* data that must not be inserted loose
  into the tree.
* In any other case → ignore only that record and carry on, accumulating a warning for
  the user.

The atomic list written by Xara LX (`Kernel/camfiltr.cpp:6996-7013`) is:

```
TAG_BEVEL(4052) TAG_BEVELINK(4057) TAG_CONTOURCONTROLLER(4066) TAG_CONTOUR(4067)
TAG_SHADOWCONTROLLER(4050) TAG_SHADOW(4051) TAG_CLIPVIEWCONTROLLER(4084) TAG_CLIPVIEW(4085)
TAG_CURRENTATTRIBUTES(4119) TAG_LIVE_EFFECT(4125) TAG_LOCKED_EFFECT(4126)
TAG_FEATHER_EFFECT(4127)
```

In the corpus there are **739 `TAG_ATOMICTAGS` records of 4 bytes each** (one per tag),
not a single record with the complete list: the reader must accumulate them all
(`Kernel/cxfile.cpp:2562-2585` iterates over `size/4` entries per record).

### 2.4. Primitive payload data types

All defined in `Kernel/cxfrec.cpp` (writing ~lines 597-1100, reading ~1148-1660).

| CXF name | Bytes | Encoding | Rust |
|---|---|---|---|
| `BYTE` | 1 | unsigned integer | `u8` |
| `UINT16` / `INT16` | 2 | LE | `u16` / `i16` |
| `UINT32` / `INT32` | 4 | LE | `u32` / `i32` |
| `REFERENCE` | 4 | LE, **signed**: >0 = record number; <0 = predefined reference; 0 = error/null | `i32` |
| `FLOAT` | 4 | IEEE-754 binary32 LE | `f32` |
| `DOUBLE` | 8 | IEEE-754 binary64 LE | `f64` |
| `FIXED16` | 4 | `i32` with the binary point between bits 15 and 16 → real value = `raw / 65536.0` (`Kernel/ccmaths.h:116`, `Kernel/fixed16.h`) | `i32` + helper |
| `ANGLE` | 4 | alias of `FIXED16`, in radians (`Kernel/ccmaths.h:120`) | `i32` + helper |
| `FIXED24` | 4 | `i32` with 24 fractional bits → `raw / 16777216.0` (`Kernel/fixed24.h:157,264`). Appears only in colour components | `i32` + helper |
| `DocCoord` | 8 | two `INT32`s (x, y) in millipoints (§5) | `(i32,i32)` |
| interleaved `DocCoord` | 8 | alternating x/y bytes (§7.3) | same |
| `Matrix` | 24 | `FIXED16 a, b, c, d` + `INT32 e, f` (`Kernel/cxfrec.cpp:1913-1959`) | see §5.4 |
| `ASCII-Z` | var | ASCII bytes terminated by `0x00` (`Kernel/cxfrec.cpp:1546-1562`) | `CString`-like |
| `UNICODE-Z` | var | UTF-16 **LE**, terminated by `0x0000` (2 bytes). Constant `SIZEOF_XAR_UTF16 = 2` (`Kernel/cxfile.h:127`) | `Vec<u16>` → `String` |
| `UTF16STR` | var | identical to `UNICODE-Z` but with no length limit (`Kernel/cxfrec.cpp:1034-1058`) | same |
| `CCPanose` | 10 | 10 PANOSE bytes (family, serif, weight, proportion, contrast, strokeVar, armStyle, letterform, midline, xHeight) (`Kernel/cxfrec.cpp:1416-1447`) | `[u8;10]` |
| `RGBTRIPLE` | 3 | R, G, B (only in `TAG_DEFINEBITMAP_JPEG8BPP` palettes) | `[u8;3]` |

> **Caution:** `TAG_TEXT_STRING` (2201) is the only string that carries **no** terminator:
> its length is `size / 2` UTF-16 characters. Verified in `TextDesigns/SimpleText.xar`
> (size 38 = 19 characters, "Single line of text", no NUL).

### 2.5. Rust pseudocode for the record reader

```rust
pub const XAR_MAGIC: [u8; 8] = [0x58, 0x41, 0x52, 0x41, 0xA3, 0xA3, 0x0D, 0x0A];

#[derive(Debug, Clone)]
pub struct Record {
    pub number: u32,   // 1..N, order in the file; it is the key used by references
    pub tag: u32,
    pub data: Vec<u8>, // length == size
}

/// Cursor over a record payload: EVERYTHING is little-endian.
pub struct Cur<'a> { b: &'a [u8], p: usize }

impl<'a> Cur<'a> {
    pub fn new(b: &'a [u8]) -> Self { Cur { b, p: 0 } }
    pub fn remaining(&self) -> usize { self.b.len() - self.p }
    fn take(&mut self, n: usize) -> Result<&'a [u8], Err> {
        if self.remaining() < n { return Err(Err::Eof); }
        let s = &self.b[self.p..self.p + n]; self.p += n; Ok(s)
    }
    pub fn u8(&mut self)  -> Result<u8, Err>  { Ok(self.take(1)?[0]) }
    pub fn u16(&mut self) -> Result<u16, Err> { Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap())) }
    pub fn i16(&mut self) -> Result<i16, Err> { Ok(self.u16()? as i16) }
    pub fn u32(&mut self) -> Result<u32, Err> { Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap())) }
    pub fn i32(&mut self) -> Result<i32, Err> { Ok(self.u32()? as i32) }
    pub fn f32(&mut self) -> Result<f32, Err> { Ok(f32::from_le_bytes(self.take(4)?.try_into().unwrap())) }
    pub fn f64(&mut self) -> Result<f64, Err> { Ok(f64::from_le_bytes(self.take(8)?.try_into().unwrap())) }

    pub fn fixed16(&mut self) -> Result<f64, Err> { Ok(self.i32()? as f64 / 65536.0) }
    pub fn fixed24(&mut self) -> Result<f64, Err> { Ok(self.i32()? as f64 / 16_777_216.0) }
    pub fn angle(&mut self)   -> Result<f64, Err> { self.fixed16() }          // radians
    pub fn reference(&mut self) -> Result<i32, Err> { self.i32() }

    pub fn coord(&mut self) -> Result<Coord, Err> { Ok(Coord { x: self.i32()?, y: self.i32()? }) }

    /// 8 bytes with the bytes of X and Y interleaved, from MSB to LSB.
    pub fn coord_interleaved(&mut self) -> Result<Coord, Err> {
        let b = self.take(8)?;
        let x = i32::from_be_bytes([b[0], b[2], b[4], b[6]]);
        let y = i32::from_be_bytes([b[1], b[3], b[5], b[7]]);
        Ok(Coord { x, y })
    }

    pub fn matrix(&mut self) -> Result<Matrix, Err> {
        Ok(Matrix { a: self.fixed16()?, b: self.fixed16()?, c: self.fixed16()?,
                    d: self.fixed16()?, e: self.i32()?,     f: self.i32()? })
    }

    pub fn ascii_z(&mut self) -> Result<String, Err> {
        let mut v = Vec::new();
        loop { let c = self.u8()?; if c == 0 { break } v.push(c); }
        Ok(String::from_utf8_lossy(&v).into_owned())
    }

    pub fn utf16_z(&mut self) -> Result<String, Err> {
        let mut v = Vec::new();
        loop { let c = self.u16()?; if c == 0 { break } v.push(c); }
        Ok(String::from_utf16_lossy(&v))
    }

    /// The rest of the record as UTF-16 with no terminator (TAG_TEXT_STRING).
    pub fn utf16_rest(&mut self) -> Result<String, Err> {
        let mut v = Vec::new();
        while self.remaining() >= 2 { let c = self.u16()?; if c == 0 { break } v.push(c); }
        Ok(String::from_utf16_lossy(&v))
    }
}
```

---

## 3. Compression

### 3.1. What is compressed and what is not

* The 8 signature bytes: **never** compressed.
* `TAG_FILEHEADER` and, typically, the preview bitmap: **uncompressed**
  (so that a viewer can read them without inflating anything).
* From the end of the `TAG_STARTCOMPRESSION` record (tag 30) to the end of the
  `TAG_ENDCOMPRESSION` record (tag 31): **raw deflate stream**.
* The **"streamed" records** (bitmap and sound definitions) **force the compressed block
  to close before they are written and reopen it afterwards**
  (`Kernel/cxfile.cpp:1437-1465` and `Kernel/cxfile.cpp:1550-1570`). That is why the
  corpus has 103 START/END pairs across 59 files: documents with bitmaps have several
  compressed blocks.

A generic parser **does not need to know about streamed records**: implementing START/END
correctly is enough and everything falls into place naturally. (Confirmed: the validation
parser in §12.1 processes all 59 files without exceptions, including those with embedded
JPEG/PNG.)

### 3.2. `TAG_STARTCOMPRESSION` (30), 4-byte payload

`Kernel/cxfile.cpp:705-742`:

The original writer composes the value as `major * 100 + minor` from the two zlib version
constants carried by the tree itself, and emits it as a single `u32`.

| Offset | Type | Field |
|---|---|---|
| 0 | `u32` | `compression_version`: high byte = **compression type** (0 = zlib/deflate, the only known value); the rest = zlib version `major*100 + minor` |

`Kernel/zutil.h:144-145` defines `ZLIB_MAJOR_VERSIONNO = 0`, `ZLIB_MINOR_VERSIONNO = 99`,
so the value written is always **99** (`0x00000063`). In all 59 files of the corpus the
value is 99 without exception. The original reader only *traces* this value and does not
validate it (`Kernel/cxfile.cpp:660-670`); checking that the high byte is 0 and warning
otherwise is recommended.

Immediately **after** the 4 payload bytes the deflate stream begins.

### 3.3. The exact algorithm

`Kernel/zstream.cpp:490-545` (initialisation) and `Kernel/zstream.cpp:760-770`:

The original initialises zlib with the following parameters (the names are those of
zlib's public API, not of the original):

| Direction | Parameter | Value used | Effect |
|---|---|---|---|
| writing | `level` | `Z_DEFAULT_COMPRESSION` (6) | irrelevant for the reader |
| writing | `method` | `Z_DEFLATED` | DEFLATE |
| both | `windowBits` | `-MAX_WBITS` (`-15`) | **raw stream**: no zlib or gzip header |
| writing | `memLevel` | `DEF_MEM_LEVEL` (8) | compressor memory usage |
| writing | `strategy` | `Z_DEFAULT_STRATEGY` (0) | default strategy |

* Algorithm: **raw DEFLATE (RFC 1951), with no zlib or gzip header** (negative
  `windowBits` = `-15`).
* Level: `Z_DEFAULT_COMPRESSION` (6). Irrelevant for the reader.
* Strategy: default; `memLevel` = `DEF_MEM_LEVEL` (8).
* In Rust: `flate2::Decompress::new(false)` / `DeflateDecoder`, or
  `miniz_oxide::inflate::stream` with `wrap = false`.

> There is a variant with a zlib header (positive `MAX_WBITS`) in `gz_init` when the
> `Header` parameter is true (`Kernel/zstream.cpp:493-500`), but **it is not used for
> `.xar`**: `CCStreamFile::InitCompression(Header=FALSE)` is the path taken by the native
> filter (`Kernel/ccfile.h:636`, `Kernel/cxfile.cpp:738`).

### 3.4. `TAG_ENDCOMPRESSION` (31), 8-byte payload = CRC + size

This is the subtlest part of the format, and it has to be implemented exactly
(`Kernel/cxfile.cpp:789-816` + `Kernel/zstream.cpp:1256-1275`):

1. The **header** of the `TAG_ENDCOMPRESSION` record (tag=31, size=8) is written **inside
   the compressed stream** (via `WriteRecordHeader`, which writes through the active
   stream).
2. Immediately afterwards the deflate is closed (`Z_FINISH` + flush) and 8 bytes are
   written **uncompressed, directly into the file**:

| Offset | Type | Field |
|---|---|---|
| 0 | `u32` LE | `crc32` of **all the uncompressed bytes** of the block |
| 4 | `u32` LE | number of **uncompressed** bytes in the block (`stream.total_in`) |

`putLong` writes in LSB-first order (`Kernel/zstream.cpp:1205-1217`), and `getLong` reads
it the same way (`Kernel/zstream.cpp:1228-1240`).

The CRC is the **standard zlib CRC-32** (polynomial 0xEDB88320, init 0, final xorout;
`Kernel/zstream.cpp:1550+`, the classic zlib table), computed over the *plain* content
(what the reader obtains once inflated), including the 8-byte header of the
`TAG_ENDCOMPRESSION` record itself.

The check performed by the original reader (`Kernel/zstream.cpp:1298-1310`) is:
`crc_read == crc_computed && size_read == total_out`.

**Empirical validation:** the parser in §12.1 recomputes the CRC and the length across all
59 files of the corpus and matches on 100 % of the 103 compressed blocks.

### 3.5. Decompression state machine (Rust)

```rust
enum Stream { Plain, Deflate(flate2::Decompress) }

pub struct XarReader<'a> {
    raw: &'a [u8],
    pos: usize,          // position in the physical file
    inflated: Vec<u8>,   // output buffer of the inflate
    ipos: usize,         // position within `inflated`
    st: Stream,
    crc: u32,            // accumulated CRC of the current block
    total_in: u32,       // inflated bytes of the current block
}

impl<'a> XarReader<'a> {
    fn read(&mut self, n: usize) -> Result<Vec<u8>, Err> { /* reads from raw or from inflated */ }

    fn start_compression(&mut self) {
        self.st = Stream::Deflate(flate2::Decompress::new(/*zlib_header=*/false));
        self.inflated.clear(); self.ipos = 0; self.crc = 0; self.total_in = 0;
    }

    /// Called AFTER the header (tag=31,size=8) has been read from the inflated stream.
    fn end_compression(&mut self) -> Result<(), Err> {
        let (crc_calc, len_calc) = (self.crc, self.total_in);
        // The 8-byte trailer is UNCOMPRESSED in the file, right after
        // the last byte consumed by the inflater.
        self.pos = self.deflate_end_offset();      // the inflater's total_in over `raw`
        self.st = Stream::Plain;
        let t = self.read(8)?;
        let crc_file = u32::from_le_bytes(t[0..4].try_into().unwrap());
        let len_file = u32::from_le_bytes(t[4..8].try_into().unwrap());
        if crc_file != crc_calc || len_file != len_calc { warn!("corrupt compressed block"); }
        Ok(())
    }
}
```

**Important implementation note:** you need to know *how many bytes of the physical file
the inflater has actually consumed* in order to position yourself at the trailer. With
`flate2` you use `Decompress::total_in()`; with the high-level API you can read
`unused_data`/`into_inner()`. The original code makes exactly the same adjustment
(`Kernel/zstream.cpp:1312-1318`: it rewinds the pointer by `n - 8` bytes).

### 3.6. "Streamed" records (bitmaps)

`Kernel/cxfile.cpp:1437-1465` (`StartStreamedRecord`) and `Kernel/cxfile.cpp:1593-1612`
(`FixStreamedRecordHeader`):

* Compression is switched off (emitting the corresponding `TAG_ENDCOMPRESSION`).
* The `tag/size` header is written **uncompressed**, with a provisional `size`.
* The content (a complete PNG/JPEG/GIF/BMP/WAV file) is dumped **uncompressed**.
* The file pointer is repositioned and the `size` field is patched
  (`Size = Pos - StartOfStreamedRecord - 8`).
* Compression is switched back on (a new `TAG_STARTCOMPRESSION`).

For the reader there is nothing special about it: read `size` bytes.

---

## 4. Complete table of tags and layouts

### 4.0. How to read the table

The **"size"** column gives the payload size according to `Kernel/cxfdefs.h`
(`var` = variable, `—` = not declared in `cxfdefs.h`). The **"corpus"** column gives how
many times the tag appears across the 59 `.xar` files analysed (0 = **not observed**;
useful for prioritising). Every declared fixed size has been **verified against the actual
observed sizes** and they match (§12.2).

In total **300 tags** are catalogued below; **157 distinct ones** appear in the
corpus.

> **Correction (verified 2026-09-20).** An earlier revision said these 300 were
> all defined in `Kernel/cxftags.h`. They are not: that file defines **211**
> `TAG_*` names. Counting `Kernel/cxfdefs.h` and `Kernel/basedoc.h` as well
> gives **433** distinct `TAG_*` names in the original tree. The 300 here are
> the ones with a known wire meaning, gathered from all three headers. Verify
> with:
> `grep -hoE '#define[[:space:]]+TAG_[A-Za-z0-9_]+' Kernel/*.h | awk '{print $2}' | sort -u | wc -l`
>
> Two tags are missing from the table below and are documented elsewhere in
> this note: **`TAG_PATH_FLAGS` (111)**, which is 10.96 % of all corpus records
> — see §7.5 — and **`TAG_TEXT_FONT_SIZE` (2906)** — see §4's text section.
> Anyone implementing from the table alone would miss both.

### 4.1. Master table

| tag | symbolic name | size | corpus | description |
|---:|---|---|---:|---|
| 0 | `TAG_UP` | 0 | 201121 | Move up one level in the tree (closes the group of children opened by TAG_DOWN) |
| 1 | `TAG_DOWN` | 0 | 201121 | Move down one level: the following records are children of the last node inserted |
| 2 | `TAG_FILEHEADER` | var | 59 | Logical header: file type, uncompressed size, producer and version |
| 3 | `TAG_ENDOFFILE` | 0 | 59 | End of file; the parser must stop here |
| 10 | `TAG_ATOMICTAGS` | var | 739 | List of "atomic" tags: if they are not recognised, their whole subtree is discarded |
| 11 | `TAG_ESSENTIALTAGS` | var | 0 | List of "essential" tags: if they are not recognised, the import must be aborted |
| 12 | `TAG_TAGDESCRIPTION` | var | 0 | Textual descriptions of tags, for warning messages shown to the user |
| 20 | `TAG_NONRENDERSECTION_START` | 0 | 0 | Start of a non-renderable section (reserved, not implemented) |
| 21 | `TAG_NONRENDERSECTION_END` | 0 | 0 | End of a non-renderable section (reserved, not implemented) |
| 22 | `TAG_RENDERING_PAUSE` | 0 | 0 | Pause rendering (reserved, not implemented) |
| 23 | `TAG_RENDERING_RESUME` | 0 | 0 | Resume rendering (reserved, not implemented) |
| 30 | `TAG_STARTCOMPRESSION` | 4 | 103 | From the next byte on, the stream is compressed (raw deflate) |
| 31 | `TAG_ENDCOMPRESSION` | 8 | 103 | End of the compressed block; its 8 data bytes are CRC32+uncompressed size |
| 40 | `TAG_DOCUMENT` | 0 | 59 | Root node of the document |
| 41 | `TAG_CHAPTER` | 0 | 59 | Chapter node (container of spreads) |
| 42 | `TAG_SPREAD` | 0 | 59 | Spread node (sheet/spread) |
| 43 | `TAG_LAYER` | 0 | 115 | Layer node |
| 44 | `TAG_PAGE` | 16 | 0 | Page node (legacy; the geometry lives in TAG_SPREADINFORMATION) |
| 45 | `TAG_SPREADINFORMATION` | 17 | 59 | Page size, margins, bleed and spread flags |
| 46 | `TAG_GRIDRULERSETTINGS` | 17 | 59 | Grid and ruler settings for the spread |
| 47 | `TAG_GRIDRULERORIGIN` | 8 | 59 | Grid origin (DocCoord) |
| 48 | `TAG_LAYERDETAILS` | var | 115 | Layer flags and name |
| 49 | `TAG_GUIDELAYERDETAILS` | var | 0 | As LAYERDETAILS plus the colour reference for the guides |
| 50 | `TAG_DEFINERGBCOLOUR` | 3 | 0 | Simple RGB colour definition (3 bytes); referenceable by record number |
| 51 | `TAG_DEFINECOMPLEXCOLOUR` | 29+name | 5766 | Full indexed/named colour definition; referenceable |
| 52 | `TAG_SPREADSCALING_ACTIVE` | 24 | 1 | Drawing scale of the spread (active) |
| 53 | `TAG_SPREADSCALING_INACTIVE` | 24 | 58 | Drawing scale of the spread (inactive) |
| 60 | `TAG_PREVIEWBITMAP_BMP` | var | 0 | Document preview bitmap in BMP (raw data, streamed) |
| 61 | `TAG_PREVIEWBITMAP_GIF` | var | 58 | Preview bitmap in GIF (raw data, streamed) |
| 62 | `TAG_PREVIEWBITMAP_JPEG` | var | 0 | Preview bitmap in JPEG (streamed) |
| 63 | `TAG_PREVIEWBITMAP_PNG` | var | 0 | Preview bitmap in PNG (streamed) |
| 64 | `TAG_PREVIEWBITMAP_TIFFLZW` | var | 0 | Preview bitmap in TIFF-LZW (streamed) |
| 65 | `TAG_DEFINEBITMAP_BMP` | var | 0 | Bitmap definition: name + embedded BMP file (streamed, referenceable) |
| 66 | `TAG_DEFINEBITMAP_GIF` | var | 0 | Bitmap definition: name + embedded GIF file (streamed, referenceable) |
| 67 | `TAG_DEFINEBITMAP_JPEG` | var | 4 | Bitmap definition: name + embedded JPEG (streamed, referenceable) |
| 68 | `TAG_DEFINEBITMAP_PNG` | var | 32 | Bitmap definition: name + embedded PNG (streamed, referenceable) |
| 69 | `TAG_DEFINEBITMAP_BMPZIP` | var | 0 | Compressed BMP bitmap definition (streamed) |
| 70 | `TAG_DEFINESOUND_WAV` | var | 0 | Embedded WAV sound definition (streamed) |
| 71 | `TAG_DEFINEBITMAP_JPEG8BPP` | var | 8 | 24bpp JPEG + palette used to reconstruct an 8bpp bitmap (streamed) |
| 80 | `TAG_VIEWPORT` | 16 | 59 | Rectangle of the document view |
| 81 | `TAG_VIEWQUALITY` | 1 | 0 | Render quality of the view |
| 82 | `TAG_DOCUMENTVIEW` | 24 | 59 | Scale, visible area and flags of the saved view |
| 85 | `TAG_DEFINE_PREFIXUSERUNIT` | 28+strings | 0 | Definition of a user unit with a prefix; referenceable |
| 86 | `TAG_DEFINE_SUFFIXUSERUNIT` | 28+strings | 0 | Definition of a user unit with a suffix; referenceable |
| 87 | `TAG_DEFINE_DEFAULTUNITS` | 8 | 59 | Default page and font units (references to units) |
| 90 | `TAG_DOCUMENTCOMMENT` | var | 0 | Document comment (Unicode string) |
| 91 | `TAG_DOCUMENTDATES` | 8 | 59 | Creation and last-saved dates |
| 92 | `TAG_DOCUMENTUNDOSIZE` | 4 | 59 | Size of the undo buffer |
| 93 | `TAG_DOCUMENTFLAGS` | 4 | 59 | Document flags (bit0 = all layers visible, bit1 = multi-layer) |
| 95 | `TAG_NAMEGAL_DOCCOMP` | — | 0 | Name gallery document component (not implemented) |
| 100 | `TAG_PATH` | var | 0 | Absolute path, neither filled nor stroked |
| 101 | `TAG_PATH_FILLED` | var | 0 | Absolute path, filled |
| 102 | `TAG_PATH_STROKED` | var | 0 | Absolute path, stroked |
| 103 | `TAG_PATH_FILLED_STROKED` | var | 0 | Absolute path, filled and stroked |
| 104 | `TAG_GROUP` | — | 19075 | Group node (no payload); the children sit between DOWN/UP |
| 105 | `TAG_BLEND` | 3 | 480 | Blend node: number of steps and flags |
| 106 | `TAG_BLENDER` | 8 | 508 | "Blender" node that pairs up source/destination paths |
| 107 | `TAG_MOULD_ENVELOPE` | 4 | 68 | Envelope-type mould (threshold) |
| 108 | `TAG_MOULD_PERSPECTIVE` | 4 | 129 | Perspective-type mould (threshold) |
| 109 | `TAG_MOULD_GROUP` | 0 | 197 | Moulded group (container) |
| 110 | `TAG_MOULD_PATH` | var | 197 | Path defining the mould mesh |
| 112 | `TAG_GUIDELINE` | 5 | 0 | Guideline (type + ordinate) |
| 113 | `TAG_PATH_RELATIVE` | var | 0 | Path in relative/interleaved format, neither filled nor stroked |
| 114 | `TAG_PATH_RELATIVE_FILLED` | var | 0 | Relative path, filled |
| 115 | `TAG_PATH_RELATIVE_STROKED` | var | 16888 | Relative path, stroked |
| 116 | `TAG_PATH_RELATIVE_FILLED_STROKED` | var | 135227 | Relative path, filled and stroked |
| 118 | `TAG_PATHREF_TRANSFORM` | 28 | 0 | Path identical to another path record, with a matrix applied |
| 150 | `TAG_FLATFILL` | 4 | 69066 | Flat fill with a colour reference |
| 151 | `TAG_LINECOLOUR` | 4 | 63297 | Line colour with a colour reference |
| 152 | `TAG_LINEWIDTH` | 4 | 138164 | Line width in millipoints |
| 153 | `TAG_LINEARFILL` | 40 | 55864 | Linear gradient fill (2 colours + profile) |
| 154 | `TAG_CIRCULARFILL` | 40 | 108 | Circular gradient fill |
| 155 | `TAG_ELLIPTICALFILL` | 48 | 29161 | Elliptical gradient fill |
| 156 | `TAG_CONICALFILL` | 40 | 409 | Conical gradient fill |
| 157 | `TAG_BITMAPFILL` | 44 | 29 | Bitmap fill |
| 158 | `TAG_CONTONEBITMAPFILL` | 52 | 7 | Contone bitmap fill (2 colours) |
| 159 | `TAG_FRACTALFILL` | 69 | 78 | Fractal (plasma) fill |
| 160 | `TAG_FILLEFFECT_FADE` | 0 | 107 | Fill colour interpolation effect: fade |
| 161 | `TAG_FILLEFFECT_RAINBOW` | 0 | 10 | Interpolation effect: rainbow (HSV, short way round) |
| 162 | `TAG_FILLEFFECT_ALTRAINBOW` | 0 | 8 | Interpolation effect: alternative rainbow |
| 163 | `TAG_FILL_REPEATING` | 0 | 107 | Fill mapping: repeat |
| 164 | `TAG_FILL_NONREPEATING` | 0 | 155 | Fill mapping: do not repeat |
| 165 | `TAG_FILL_REPEATINGINVERTED` | 0 | 172 | Fill mapping: repeat inverted |
| 166 | `TAG_FLATTRANSPARENTFILL` | 2 | 22949 | Flat transparency (level + type) |
| 167 | `TAG_LINEARTRANSPARENTFILL` | 35 | 11797 | Linear graduated transparency |
| 168 | `TAG_CIRCULARTRANSPARENTFILL` | 35 | 45 | Circular graduated transparency |
| 169 | `TAG_ELLIPTICALTRANSPARENTFILL` | 43 | 5279 | Elliptical graduated transparency |
| 170 | `TAG_CONICALTRANSPARENTFILL` | 35 | 5 | Conical graduated transparency |
| 171 | `TAG_BITMAPTRANSPARENTFILL` | 47 | 3 | Bitmap transparency |
| 172 | `TAG_FRACTALTRANSPARENTFILL` | 64 | 4 | Fractal transparency |
| 173 | `TAG_LINETRANSPARENCY` | 2 | 24245 | Line transparency (level + type) |
| 174 | `TAG_STARTCAP` | 1 | 26258 | Start line cap (butt/round/square) |
| 175 | `TAG_ENDCAP` | 1 | 26258 | End line cap |
| 176 | `TAG_JOINSTYLE` | 1 | 65951 | Segment join style (mitre/round/bevel) |
| 177 | `TAG_MITRELIMIT` | 4 | 0 | Mitre limit |
| 178 | `TAG_WINDINGRULE` | 1 | 3 | Fill rule (nonzero/negative/evenodd/positive) |
| 179 | `TAG_QUALITY` | 4 | 0 | Render quality of the object |
| 180 | `TAG_TRANSPARENTFILL_REPEATING` | 0 | 107 | Transparency mapping: repeat |
| 181 | `TAG_TRANSPARENTFILL_NONREPEATING` | 0 | 5 | Transparency mapping: do not repeat |
| 182 | `TAG_TRANSPARENTFILL_REPEATINGINVERTED` | 0 | 19 | Transparency mapping: repeat inverted |
| 183 | `TAG_DASHSTYLE` | 4 | 15 | Dash pattern by reference (negative = predefined pattern) |
| 184 | `TAG_DEFINEDASH` | var | 0 | Explicit dash pattern (start, width, number of elements, array) |
| 185 | `TAG_ARROWHEAD` | 12 | 12 | Start arrowhead: reference + width/height scale |
| 186 | `TAG_ARROWTAIL` | 12 | 50 | End arrowhead: reference + width/height scale |
| 187 | `TAG_DEFINEARROW` | — | 0 | Custom arrowhead definition (declared, not implemented) |
| 188 | `TAG_DEFINEDASH_SCALED` | var | 0 | Explicit dash pattern that scales with the line width |
| 189 | `TAG_USERVALUE` | var | 0 | Key/value user attribute (Unicode) |
| 190 | `TAG_FLATFILL_NONE` | 0 | 0 | Flat fill "none" (no payload) |
| 191 | `TAG_FLATFILL_BLACK` | 0 | 0 | Black flat fill (no payload) |
| 192 | `TAG_FLATFILL_WHITE` | 0 | 0 | White flat fill (no payload) |
| 193 | `TAG_LINECOLOUR_NONE` | 0 | 74003 | Line colour "none" (no payload) |
| 194 | `TAG_LINECOLOUR_BLACK` | 0 | 0 | Black line colour (no payload) |
| 195 | `TAG_LINECOLOUR_WHITE` | 0 | 0 | White line colour (no payload) |
| 198 | `TAG_NODE_BITMAP` | 36 | 25 | Bitmap object: 4-point parallelogram + bitmap reference |
| 199 | `TAG_NODE_CONTONEDBITMAP` | 44 | 0 | Contoned bitmap object: 4 points + bitmap + 2 colours |
| 200 | `TAG_SQUAREFILL` | 48 | 19 | Square gradient fill |
| 201 | `TAG_SQUARETRANSPARENTFILL` | 43 | 32 | Square graduated transparency |
| 202 | `TAG_THREECOLFILL` | 36 | 8 | Three-colour gradient fill |
| 203 | `TAG_THREECOLTRANSPARENTFILL` | 28 | 2 | Three-level transparency |
| 204 | `TAG_FOURCOLFILL` | 40 | 13 | Four-colour gradient fill |
| 205 | `TAG_FOURCOLTRANSPARENTFILL` | 29 | 2 | Four-level transparency |
| 206 | `TAG_FILL_REPEATING_EXTRA` | 0 | 10 | Fill mapping: "extra" repeat |
| 207 | `TAG_TRANSPARENTFILL_REPEATING_EXTRA` | 0 | 0 | Transparency mapping: "extra" repeat |
| 1000 | `TAG_ELLIPSE_SIMPLE` | 16 | 0 | Simple ellipse (centre, width, height) |
| 1001 | `TAG_ELLIPSE_COMPLEX` | 24 | 0 | Complex ellipse (centre, major axis, minor axis) |
| 1100 | `TAG_RECTANGLE_SIMPLE` | 16 | 0 | Simple rectangle (centre/width/height) |
| 1101 | `TAG_RECTANGLE_SIMPLE_REFORMED` | var | 0 | Simple rectangle (centre/width/height), with edited edge paths |
| 1102 | `TAG_RECTANGLE_SIMPLE_STELLATED` | 32 | 0 | Simple rectangle (centre/width/height), stellated |
| 1103 | `TAG_RECTANGLE_SIMPLE_STELLATED_REFORMED` | var | 0 | Simple rectangle (centre/width/height), stellated, with edited edge paths |
| 1104 | `TAG_RECTANGLE_SIMPLE_ROUNDED` | 24 | 0 | Simple rectangle (centre/width/height), rounded corners |
| 1105 | `TAG_RECTANGLE_SIMPLE_ROUNDED_REFORMED` | var | 0 | Simple rectangle (centre/width/height), rounded corners, with edited edge paths |
| 1106 | `TAG_RECTANGLE_SIMPLE_ROUNDED_STELLATED` | 48 | 0 | Simple rectangle (centre/width/height), rounded corners, stellated |
| 1107 | `TAG_RECTANGLE_SIMPLE_ROUNDED_STELLATED_REFORMED` | var | 0 | Simple rectangle (centre/width/height), rounded corners, stellated, with edited edge paths |
| 1108 | `TAG_RECTANGLE_COMPLEX` | 24 | 0 | Complex rectangle (centre/axes) |
| 1109 | `TAG_RECTANGLE_COMPLEX_REFORMED` | var | 0 | Complex rectangle (centre/axes), with edited edge paths |
| 1110 | `TAG_RECTANGLE_COMPLEX_STELLATED` | 40 | 0 | Complex rectangle (centre/axes), stellated |
| 1111 | `TAG_RECTANGLE_COMPLEX_STELLATED_REFORMED` | var | 0 | Complex rectangle (centre/axes), stellated, with edited edge paths |
| 1112 | `TAG_RECTANGLE_COMPLEX_ROUNDED` | 32 | 0 | Complex rectangle (centre/axes), rounded corners |
| 1113 | `TAG_RECTANGLE_COMPLEX_ROUNDED_REFORMED` | var | 0 | Complex rectangle (centre/axes), rounded corners, with edited edge paths |
| 1114 | `TAG_RECTANGLE_COMPLEX_ROUNDED_STELLATED` | 56 | 0 | Complex rectangle (centre/axes), rounded corners, stellated |
| 1115 | `TAG_RECTANGLE_COMPLEX_ROUNDED_STELLATED_REFORMED` | var | 0 | Complex rectangle (centre/axes), rounded corners, stellated, with edited edge paths |
| 1200 | `TAG_POLYGON_COMPLEX` | 26 | 0 | Complex polygon (number of sides, centre, axes) |
| 1201 | `TAG_POLYGON_COMPLEX_REFORMED` | var | 0 | Complex polygon (centre/axes), with edited edge paths |
| 1212 | `TAG_POLYGON_COMPLEX_STELLATED` | 42 | 0 | Complex polygon (centre/axes), stellated |
| 1213 | `TAG_POLYGON_COMPLEX_STELLATED_REFORMED` | var | 0 | Complex polygon (centre/axes), stellated, with edited edge paths |
| 1214 | `TAG_POLYGON_COMPLEX_ROUNDED` | 34 | 0 | Complex polygon (centre/axes), rounded corners |
| 1215 | `TAG_POLYGON_COMPLEX_ROUNDED_REFORMED` | var | 0 | Complex polygon (centre/axes), rounded corners, with edited edge paths |
| 1216 | `TAG_POLYGON_COMPLEX_ROUNDED_STELLATED` | 58 | 0 | Complex polygon (centre/axes), rounded corners, stellated |
| 1217 | `TAG_POLYGON_COMPLEX_ROUNDED_STELLATED_REFORMED` | var | 0 | Complex polygon (centre/axes), rounded corners, stellated, with edited edge paths |
| 1900 | `TAG_REGULAR_SHAPE_PHASE_1` | var | 0 | Generic regular shape v1 (includes the UT centre; legacy) |
| 1901 | `TAG_REGULAR_SHAPE_PHASE_2` | var | 35206 | Generic regular shape v2: flags, sides, axes, matrix, stellation, rounding and paths |
| 2000 | `TAG_FONT_DEF_TRUETYPE` | var | 61 | TrueType font definition: full name, typeface and PANOSE; referenceable |
| 2001 | `TAG_FONT_DEF_ATM` | var | 0 | ATM/Type1 font definition; referenceable |
| 2100 | `TAG_TEXT_STORY_SIMPLE` | 12 | 140 | Text story with a simple position (DocCoord + autokern) |
| 2101 | `TAG_TEXT_STORY_COMPLEX` | 28 | 25 | Text story with a full matrix (+ autokern) |
| 2110 | `TAG_TEXT_STORY_SIMPLE_START_LEFT` | 12 | 2 | Text story on a path (simple start left) |
| 2111 | `TAG_TEXT_STORY_SIMPLE_START_RIGHT` | 12 | 0 | Text story on a path (simple start right) |
| 2112 | `TAG_TEXT_STORY_SIMPLE_END_LEFT` | 12 | 0 | Text story on a path (simple end left) |
| 2113 | `TAG_TEXT_STORY_SIMPLE_END_RIGHT` | 12 | 0 | Text story on a path (simple end right) |
| 2114 | `TAG_TEXT_STORY_COMPLEX_START_LEFT` | 36 | 3 | Text story on a path (complex start left) |
| 2115 | `TAG_TEXT_STORY_COMPLEX_START_RIGHT` | 36 | 0 | Text story on a path (complex start right) |
| 2116 | `TAG_TEXT_STORY_COMPLEX_END_LEFT` | 36 | 0 | Text story on a path (complex end left) |
| 2117 | `TAG_TEXT_STORY_COMPLEX_END_RIGHT` | 36 | 2 | Text story on a path (complex end right) |
| 2150 | `TAG_TEXT_STORY_WORD_WRAP_INFO` | 5 | 172 | Column width and word-wrap flag |
| 2151 | `TAG_TEXT_STORY_INDENT_INFO` | 8 | 172 | Left and right indents of the story |
| 2200 | `TAG_TEXT_LINE` | 0 | 328 | Text line (container, no payload) |
| 2201 | `TAG_TEXT_STRING` | var | 381 | Text string (UTF-16LE, no terminator) |
| 2202 | `TAG_TEXT_CHAR` | 2 | 67 | A single text character (UTF-16) |
| 2203 | `TAG_TEXT_EOL` | 0 | 256 | End of line/paragraph |
| 2204 | `TAG_TEXT_KERN` | 8 | 56 | Manual kern (x,y offset) |
| 2205 | `TAG_TEXT_CARET` | 0 | 0 | Text caret position |
| 2206 | `TAG_TEXT_LINE_INFO` | 12 | 328 | Line metrics: width, height and distance to the previous line |
| 2900 | `TAG_TEXT_LINESPACE_RATIO` | 4 | 93 | Proportional line spacing (FIXED16) |
| 2901 | `TAG_TEXT_LINESPACE_ABSOLUTE` | 4 | 1 | Absolute line spacing (millipoints) |
| 2902 | `TAG_TEXT_JUSTIFICATION_LEFT` | 0 | 0 | Left justification |
| 2903 | `TAG_TEXT_JUSTIFICATION_CENTRE` | 0 | 30 | Centred justification |
| 2904 | `TAG_TEXT_JUSTIFICATION_RIGHT` | 0 | 5 | Right justification |
| 2905 | `TAG_TEXT_JUSTIFICATION_FULL` | 0 | 3 | Full justification |
| 2907 | `TAG_TEXT_FONT_TYPEFACE` | 4 | 249 | Reference to the font definition record |
| 2908 | `TAG_TEXT_BOLD_ON` | 0 | 61 | Bold on |
| 2909 | `TAG_TEXT_BOLD_OFF` | 0 | 0 | Bold off |
| 2910 | `TAG_TEXT_ITALIC_ON` | 0 | 35 | Italic on |
| 2911 | `TAG_TEXT_ITALIC_OFF` | 0 | 0 | Italic off |
| 2912 | `TAG_TEXT_UNDERLINE_ON` | 0 | 0 | Underline on |
| 2913 | `TAG_TEXT_UNDERLINE_OFF` | 0 | 0 | Underline off |
| 2914 | `TAG_TEXT_SCRIPT_ON` | 8 | 0 | Super/subscript with explicit offset and size |
| 2915 | `TAG_TEXT_SCRIPT_OFF` | 0 | 0 | End of super/subscript |
| 2916 | `TAG_TEXT_SUPERSCRIPT_ON` | 0 | 6 | Superscript with implicit values |
| 2917 | `TAG_TEXT_SUBSCRIPT_ON` | 0 | 6 | Subscript with implicit values |
| 2918 | `TAG_TEXT_TRACKING` | 4 | 29 | Tracking (inter-character spacing) |
| 2919 | `TAG_TEXT_ASPECT_RATIO` | 4 | 77 | Horizontal aspect ratio of the text (FIXED16) |
| 2920 | `TAG_TEXT_BASELINE` | 4 | 101 | Baseline shift (millipoints) |
| 3500 | `TAG_OVERPRINTLINEON` | 0 | 0 | Line overprint on |
| 3501 | `TAG_OVERPRINTLINEOFF` | 0 | 0 | Line overprint off |
| 3502 | `TAG_OVERPRINTFILLON` | 0 | 0 | Fill overprint on |
| 3503 | `TAG_OVERPRINTFILLOFF` | 0 | 0 | Fill overprint off |
| 3504 | `TAG_PRINTONALLPLATESON` | 0 | 0 | Print on all plates |
| 3505 | `TAG_PRINTONALLPLATESOFF` | 0 | 0 | Do not print on all plates |
| 3506 | `TAG_PRINTERSETTINGS` | 45 | 58 | Print settings of the document |
| 3507 | `TAG_IMAGESETTING` | 15 | 58 | Imagesetting parameters (resolution, screen, function) |
| 3508 | `TAG_COLOURPLATE` | 22 | 0 | Colour plate definition (type, spot colour, angle, frequency) |
| 3509 | `TAG_PRINTMARKDEFAULT` | 1 | 228 | Predefined print mark (1 identifier byte) |
| 3510 | `TAG_PRINTMARKCUSTOM` | var | 0 | Custom print mark (subtree) |
| 4000 | `TAG_VARIABLEWIDTHFUNC` | 4 | 0 | Variable-width function of the stroke (predefined id) |
| 4001 | `TAG_VARIABLEWIDTHTABLE` | var | 0 | Variable-width table of the stroke |
| 4002 | `TAG_STROKETYPE` | 4 | 0 | Stroke type (handle to a stroke definition) |
| 4003 | `TAG_STROKEDEFINITION` | var | 0 | Stroke definition: handle, flags, repeats |
| 4004 | `TAG_STROKEAIRBRUSH` | var | 0 | Airbrush data of the stroke |
| 4010 | `TAG_NOISEFILL` | 61 | 112 | Fractal noise fill |
| 4011 | `TAG_NOISETRANSPARENTFILL` | 56 | 2 | Fractal noise transparency |
| 4012 | `TAG_MOULD_BOUNDS` | 16 | 197 | Rectangle of the original moulded object |
| 4013 | `TAG_PATHREF_IDENTICAL` | — | 0 | Path identical to another path record (reference only) |
| 4014 | `TAG_PATHREF_TRANSLATE` | — | 0 | Path equal to another plus a translation (defined, not implemented) |
| 4015 | `TAG_EXPORT_HINT` | — | 6 | Export hint (type, size, bpp, options) |
| 4020 | `TAG_WEBADDRESS` | var | 0 | Hyperlink attribute (URL + frame) |
| 4021 | `TAG_WEBADDRESS_BOUNDINGBOX` | var | 0 | Hyperlink with a bounding box |
| 4030 | `TAG_LAYER_FRAMEPROPS` | 5 | 1 | Animation frame properties of the layer (delay + flags) |
| 4031 | `TAG_SPREAD_ANIMPROPS` | 28 | 59 | Animation properties of the spread (loop, delay, palette…) |
| 4040 | `TAG_WIZOP` | var | 6 | Template/WizOp properties (Unicode strings) |
| 4041 | `TAG_WIZOP_STYLE` | — | 0 | Template style definition |
| 4042 | `TAG_WIZOP_STYLEREF` | 4 | 0 | Reference to a template style |
| 4050 | `TAG_SHADOWCONTROLLER` | 29 | 98 | Shadow controller: type, penumbra, offset, angle, scale… |
| 4051 | `TAG_SHADOW` | 24 | 98 | Shadow node: profile bias/gain and darkness |
| 4052 | `TAG_BEVEL` | 24 | 22 | Bevel controller: type, indent, light angle, outer, contrast, tilt |
| 4053 | `TAG_BEVATTR_INDENT` | 4 | 0 | Bevel attribute: indent |
| 4054 | `TAG_BEVATTR_LIGHTANGLE` | 8 | 0 | Bevel attribute: light angle |
| 4055 | `TAG_BEVATTR_CONTRAST` | — | 0 | Bevel attribute: contrast |
| 4056 | `TAG_BEVATTR_TYPE` | 4 | 0 | Bevel attribute: type |
| 4057 | `TAG_BEVELINK` | — | 22 | Bevel ink node (no payload) |
| 4060 | `TAG_BLENDER_CURVEPROP` | 16 | 0 | Travel proportions of the blend along a curve |
| 4061 | `TAG_BLEND_PATH` | var | 0 | Path along which the blend runs |
| 4062 | `TAG_BLENDER_CURVEANGLES` | 16 | 0 | Start/end angles of the blend along a curve |
| 4063 | `TAG_GROUPTRANSP` | — | 0 | Group with transparency (declared, not implemented) |
| 4064 | `TAG_CACHEBMP` | — | 0 | Cached bitmap (declared, not implemented) |
| 4065 | `TAG_CACHEDNODESGROUP` | — | 0 | Group of cached nodes (declared, not implemented) |
| 4066 | `TAG_CONTOURCONTROLLER` | 41 | 4 | Contour controller: steps, width, type and profiles |
| 4067 | `TAG_CONTOUR` | 0 | 4 | Generated contour node (no payload) |
| 4070 | `TAG_SETSENTINEL` | 0 | 59 | Sentinel for the name gallery "sets" |
| 4071 | `TAG_SETPROPERTY` | var | 0 | Property of a named set |
| 4072 | `TAG_BLENDPROFILES` | 48 | 480 | Bias/gain profiles of the blend (object, attribute, position) |
| 4073 | `TAG_BLENDERADDITIONAL` | 17 | 508 | Extra blender data (indices, curve, flags) |
| 4074 | `TAG_NODEBLENDPATH_FILLED` | 4 | 0 | Indicates whether the blend path is filled |
| 4075 | `TAG_LINEARFILLMULTISTAGE` | var | 60 | Multi-stage linear gradient (colour ramp) |
| 4076 | `TAG_CIRCULARFILLMULTISTAGE` | var | 4 | Multi-stage circular gradient |
| 4077 | `TAG_ELLIPTICALFILLMULTISTAGE` | var | 2 | Multi-stage elliptical gradient |
| 4078 | `TAG_CONICALFILLMULTISTAGE` | var | 6 | Multi-stage conical gradient |
| 4079 | `TAG_BRUSHATTR` | 33 | 2 | Brush attribute applied to an object |
| 4080 | `TAG_BRUSHDEFINITION` | 4 | 2 | Brush definition (handle) |
| 4081 | `TAG_BRUSHDATA` | var | 2 | Brush data |
| 4082 | `TAG_MOREBRUSHDATA` | 68 | 2 | Additional brush data |
| 4083 | `TAG_MOREBRUSHATTR` | 72 | 2 | Additional brush attributes |
| 4084 | `TAG_CLIPVIEWCONTROLLER` | 0 | 0 | ClipView (clipping) controller |
| 4085 | `TAG_CLIPVIEW` | 0 | 0 | ClipView node |
| 4086 | `TAG_FEATHER` | 20 | 75 | Feather attribute: size and profile |
| 4087 | `TAG_BARPROPERTY` | var | 59 | Navigation bar properties |
| 4088 | `TAG_SQUAREFILLMULTISTAGE` | var | 2 | Multi-stage square gradient |
| 4102 | `TAG_EVENMOREBRUSHDATA` | 8 | 2 | Yet more brush data |
| 4103 | `TAG_EVENMOREBRUSHATTR` | 9 | 2 | Yet more brush attributes |
| 4104 | `TAG_TIMESTAMPBRUSHDATA` | var | 0 | Brush timestamps |
| 4105 | `TAG_BRUSHPRESSUREINFO` | 28 | 2 | Brush pressure information |
| 4106 | `TAG_BRUSHPRESSUREDATA` | var | 0 | Brush pressure data |
| 4107 | `TAG_BRUSHATTRPRESSUREINFO` | 28 | 2 | Pressure information of the brush attribute |
| 4108 | `TAG_BRUSHCOLOURDATA` | — | 0 | Brush colour data (declared, not implemented) |
| 4109 | `TAG_BRUSHPRESSURESAMPLEDATA` | var | 0 | Brush pressure samples |
| 4110 | `TAG_BRUSHTIMESAMPLEDATA` | — | 0 | Brush time samples (declared, not implemented) |
| 4111 | `TAG_BRUSHATTRFILLFLAGS` | 1 | 2 | Fill flags of the brush attribute |
| 4112 | `TAG_BRUSHTRANSPINFO` | 40 | 2 | Brush transparency information |
| 4113 | `TAG_BRUSHATTRTRANSPINFO` | 40 | 2 | Transparency information of the brush attribute |
| 4114 | `TAG_DOCUMENTNUDGE` | 4 | 59 | Keyboard nudge size (millipoints) |
| 4115 | `TAG_BITMAP_PROPERTIES` | 12 | 44 | Properties of a bitmap (reference + flags) |
| 4116 | `TAG_DOCUMENTBITMAPSMOOTHING` | 5 | 59 | Bitmap smoothing for the document |
| 4117 | `TAG_XPE_BITMAP_PROPERTIES` | var | 0 | Extended XPE properties of a bitmap |
| 4118 | `TAG_DEFINEBITMAP_XPE` | 0 | 0 | Marker for a bitmap generated by XPE |
| 4119 | `TAG_CURRENTATTRIBUTES` | 1 | 106 | Container of the document's current attributes (group 1=ink, 2=text) |
| 4120 | `TAG_CURRENTATTRIBUTEBOUNDS` | 16 | 298 | Bounding box associated with the current attributes |
| 4121 | `TAG_LINEARFILL3POINT` | 48 | 17 | Three-point (non-perpendicular) linear gradient |
| 4122 | `TAG_LINEARFILLMULTISTAGE3POINT` | var | 0 | Three-point multi-stage linear gradient |
| 4123 | `TAG_LINEARTRANSPARENTFILL3POINT` | 43 | 10 | Three-point linear transparency |
| 4124 | `TAG_DUPLICATIONOFFSET` | 8 | 53 | Duplication offset of the document |
| 4125 | `TAG_LIVE_EFFECT` | var | 0 | Live effect node (effect ID + editing XML) |
| 4126 | `TAG_LOCKED_EFFECT` | var | 0 | Locked (rasterised) effect |
| 4127 | `TAG_FEATHER_EFFECT` | var | 0 | Feather effect as a node |
| 4128 | `TAG_COMPOUNDRENDER` | 20 | 7 | Compound render hint + bounding box |
| 4129 | `TAG_OBJECTBOUNDS` | 16 | 0 | Bounding box of the object (2 DocCoords) |
| 4130 | `TAG_DIMENSION` | — | 0 | Dimension line (declared, not implemented) |
| 4131 | `TAG_SPREAD_PHASE2` | — | 0 | Spread v2 (declared; only mentioned in comments) |
| 4132 | `TAG_CURRENTATTRIBUTES_PHASE2` | — | 0 | Current attributes v2 (declared; only in comments) |
| 4134 | `TAG_SPREAD_FLASHPROPS` | — | 0 | Flash properties of the spread (declared, not implemented) |
| 4135 | `TAG_PRINTERSETTINGS_PHASE2` | — | 0 | Print settings v2 (declared, not implemented) |
| 4136 | `TAG_DOCUMENTINFORMATION` | — | 0 | Document information (declared, not implemented) |
| 4137 | `TAG_CLIPVIEW_PATH` | — | 0 | ClipView clipping path (declared, not implemented) |
| 4138 | `TAG_DEFINEBITMAP_PNG_REAL` | — | 0 | Real PNG (declared, not implemented) |
| 4200 | `TAG_TEXT_TAB` | 0 | 0 | Horizontal tab |
| 4201 | `TAG_TEXT_LEFT_INDENT` | 4 | 0 | Left indent of the paragraph |
| 4202 | `TAG_TEXT_FIRST_INDENT` | 4 | 0 | First-line indent |
| 4203 | `TAG_TEXT_RIGHT_INDENT` | 4 | 0 | Right indent of the paragraph |
| 4204 | `TAG_TEXT_RULER` | var | 0 | Tab ruler of the paragraph |
| 4205 | `TAG_TEXT_STORY_HEIGHT_INFO` | — | 0 | Height of the text story (declared, not implemented) |
| 4206 | `TAG_TEXT_STORY_LINK_INFO` | — | 0 | Link between text areas (declared, not implemented) |
| 4207 | `TAG_TEXT_STORY_TRANSLATION_INFO` | — | 0 | Translation of the story (declared, not implemented) |

### 4.2. Category: tree navigation and control

| tag | record | payload |
|---|---|---|
| 0 | `TAG_UP` | empty |
| 1 | `TAG_DOWN` | empty |
| 2 | `TAG_FILEHEADER` | §1.4 |
| 3 | `TAG_ENDOFFILE` | empty — the parser **must stop** here (`Kernel/cxfile.cpp:2466-2472`) |
| 10 | `TAG_ATOMICTAGS` | `u32[size/4]` — list of atomic tags (`Kernel/cxfile.cpp:2562-2585`) |
| 11 | `TAG_ESSENTIALTAGS` | `u32[size/4]` — list of essential tags (`Kernel/cxfile.cpp:2600-2624`) |
| 12 | `TAG_TAGDESCRIPTION` | `u32 num_tags`, then `num_tags` × (`u32 tag`, `UNICODE-Z description`) (`Kernel/cxfile.cpp:2640+`) |
| 30 | `TAG_STARTCOMPRESSION` | §3.2 |
| 31 | `TAG_ENDCOMPRESSION` | §3.4 |

### 4.3. Category: document, spreads, pages and layers

**`TAG_DOCUMENT` (40), `TAG_CHAPTER` (41), `TAG_SPREAD` (42), `TAG_LAYER` (43)** — empty
payload; their content hangs between `TAG_DOWN`/`TAG_UP`.

**`TAG_SPREADINFORMATION` (45), 17 bytes** — `Kernel/rechdoc.cpp:596-625`

| Off | Type | Field |
|---|---|---|
| 0 | `i32` | `width` — page width (millipoints) |
| 4 | `i32` | `height` — page height |
| 8 | `i32` | `margin` — pasteboard margin around the pages |
| 12 | `i32` | `bleed` — bleed (0 = none) |
| 16 | `u8` | `flags` |

`flags`: bit 0 = double page spread (`DoublePageSpread`), bit 1 = show page shadow.
⚠️ **Inconsistency in the original code**: the import handler uses bit 0 for DPS
(`Kernel/rechdoc.cpp:617-620`) while the debug description code uses bit 2
(`Kernel/rechdoc.cpp:1332-1336`). Follow the handler (bit 0) and treat bit 2 as unknown
(§11).

**`TAG_GRIDRULERSETTINGS` (46), 17 bytes** — `Kernel/rechdoc.cpp:1020-1035`

| Off | Type | Field |
|---|---|---|
| 0 | `i32` (REFERENCE) | unit used by the grid |
| 4 | `f64` | divisions |
| 12 | `u32` | subdivisions |
| 16 | `u8` | grid type (0 = rectangular, 1 = isometric) |

**`TAG_GRIDRULERORIGIN` (47), 8 bytes** — `DocCoord origin`.

**`TAG_LAYERDETAILS` (48) / `TAG_GUIDELAYERDETAILS` (49)** — `Kernel/rechdoc.cpp:790-800`

| Off | Type | Field |
|---|---|---|
| 0 | `u8` | `flags` |
| 1 | `UNICODE-Z` | layer name |
| … | `i32` | (only in `TAG_GUIDELAYERDETAILS`) reference to the colour of the guides |

`flags` (`Kernel/cxfdefs.h:199-205`):

| bit | value | meaning |
|---|---|---|
| 0 | 0x01 | visible |
| 1 | 0x02 | locked |
| 2 | 0x04 | printable |
| 3 | 0x08 | active |
| 4 | 0x10 | page background |
| 5 | 0x20 | background layer |

**`TAG_LAYER_FRAMEPROPS` (4030), 5 bytes** — `u32 delay` (hundredths of a second) +
`u8 flags` with `0x01`=solid, `0x02`=overlay, `0x04`=hidden (`Kernel/cxfdefs.h:207-211`,
`Kernel/rechdoc.cpp:1680-1690`).

**`TAG_SPREADSCALING_ACTIVE` (52) / `_INACTIVE` (53), 24 bytes** —
`Kernel/rechdoc.cpp:1165-1172`: `f64 drawing_scale`, `i32 drawing_units` (REFERENCE to a
unit), `f64 real_scale`, `i32 real_units`.

**`TAG_SPREAD_ANIMPROPS` (4031), 28 bytes** — 7 × `u32`:
`loop`, `global_delay`, `dither`, `web_palette`, `colours_palette`, `num_colours`,
`flags` (`Kernel/spread.cpp:3790-3806`, `Kernel/rechdoc.cpp:1600-1625`).

**`TAG_GUIDELINE` (112), 5 bytes** — `u8 type` (`GUIDELINE_HORZ`/`VERT`) + `i32 ordinate`
(read with `ReadYOrd` if horizontal, `ReadXOrd` if vertical → **the coordinate origin is
added to it**, §5.3) (`Kernel/rechdoc.cpp:965-985`).

**`TAG_CURRENTATTRIBUTES` (4119), 1 byte** — `u8 group_id`
(1 = `ATTRIBUTEGROUP_INK`, 2 = `ATTRIBUTEGROUP_TEXT`, `Kernel/cxfdefs.h:600-602`).
It is an **atomic container node**: its children (between DOWN/UP) are the document's
current attributes (the next object drawn gets them), *not* objects in the drawing and
*not* the defaults an unattributed object inherits (§8.1). An importer that only wants geometry
can skip its whole subtree.

**`TAG_CURRENTATTRIBUTEBOUNDS` (4120), 16 bytes** — `DocCoord lo`, `DocCoord hi`.

**`TAG_DUPLICATIONOFFSET` (4124), 8 bytes** — `i32 dx`, `i32 dy`.

**`TAG_SETSENTINEL` (4070)**, empty; **`TAG_SETPROPERTY` (4071)**, variable:
`UNICODE-Z name`, `i16 num_props`, then pairs (`i16 type`, property data)
(`Kernel/ngsentry.cpp:400-420`, `Kernel/rechdoc.cpp:1750-1800`).

**`TAG_BARPROPERTY` (4087)**, variable: `i32 num_bars`, then per bar
`i32 spacing`, `u8 code`, `u8 same_size` (`Kernel/rechdoc.cpp:1860-1910`).

### 4.4. Category: colour definitions

**`TAG_DEFINERGBCOLOUR` (50), 3 bytes** — `u8 r`, `u8 g`, `u8 b`
(`Kernel/colcomp.cpp:1604-1610`). **It does not appear in any file in the corpus**: Xara
always writes complex colours (or uses the predefined negative references).

**`TAG_DEFINECOMPLEXCOLOUR` (51), 29 bytes + name** — see §9.2.

### 4.5. Category: bitmaps and sound

**`TAG_DEFINEBITMAP_*` (65–71)** — **streamed** records, uncompressed
(`Kernel/bmpcomp.cpp:2096-2175` writing, `Kernel/bmpcomp.cpp:1290-1440` reading):

| Off | Type | Field |
|---|---|---|
| 0 | `UNICODE-Z` | bitmap name (e.g. `"Default"`) |
| … | *(tag 71 only, JPEG8BPP)* `u8 n_minus_1` + `RGBTRIPLE[n]` | palette used to reconstruct 8 bpp |
| … | bytes | **the complete image file, verbatim** (PNG with its `\x89PNG` signature, JPEG, GIF, BMP…) |

Tag → format mapping (`Kernel/bmpcomp.cpp:1320-1345`):
65 = BMP, 66 = GIF, 67 = JPEG, 68 = PNG, 69 = compressed (zip) BMP, 70 = WAV (sound),
71 = 24 bpp JPEG + palette (representing an original 8 bpp bitmap).

Verified in `testfiles/TestBitmapFill.xar`: tag 68, size 2177,
payload = `"Default\0"` in UTF-16 (16 bytes) followed by `89 50 4E 47 0D 0A 1A 0A …`.

**Three tags do not embed a standard file** (phase 10, 2026-09-23):

- **65 `BMP` is a headerless DIB.** The BMP import filter reads it with the
  "read a `BITMAPFILEHEADER`" flag off (`wxOil/bmpfiltr.cpp:698-707` calling
  `DIBUtil::ReadFromFile(…, FALSE, …)`; the flag's meaning is documented at
  `wxOil/dibutil.cpp:1123-1124`). The image therefore starts at the
  `BITMAPINFOHEADER` (`biSize` = 40), not at `BM`. A reader must synthesise
  the 14-byte file header, computing the pixel offset from `biSize`, the
  `BI_BITFIELDS` masks and the palette (`biClrUsed`, or `2^bpp` when it is 0
  and `bpp ≤ 8`). `xarast-image` also sniffs first, so a `BM` file stored
  under 65 by some other writer still decodes.
- **69 `BMPZIP` is the same DIB behind the file's stream compression**:
  `Compressed = TRUE` for tag 69 (`Kernel/bmpcomp.cpp:1335-1337`) makes the
  BMP filter switch the file into compressed mode around the read
  (`wxOil/bmpfiltr.cpp:698-713`, `Kernel/ccfile.cpp:200-221`). The exact
  framing is unverified — the corpus has no tag-69 record — so the reader
  accepts either a zlib stream or raw DEFLATE, bounded by the decode limits.
- **71 `JPEG8BPP`** is a 24 bpp JPEG plus the palette of the 8 bpp original
  (`Kernel/bmpcomp.cpp:1339-1342` sets `ReadPalette`). The reconstruction
  happens only when the palette has 1–256 entries and the decoded JPEG is
  24 bpp (`Kernel/bitmap.cpp:817-822`); it maps each pixel onto the palette
  **without dithering** (`wxOil/dibutil.cpp:3741` requests
  `XARADITHER_NONE`). The colour-matching metric itself runs inside the
  closed rasteriser and is not observable; `xarast-image` uses nearest
  squared RGB distance, lowest index on a tie.

**68 `PNG` stores transparency in its alpha channel** (2026-09-23). The
original's 32 bpp bitmaps carry *transparency* (0 = opaque) in the fourth
byte. Its PNG reader inverts alpha on every read (`wxOil/pngutil.cpp:320`,
`png_set_invert_alpha`); the native-file path then inverts it back
(`wxOil/pngfiltr.cpp:385-394`, `PNGFilter::ReadFromFile` with a filter), and
the native writer inverts twice (`wxOil/pngfiltr.cpp:846-856` then
`:919-926`). Net effect: a PNG embedded under tag 68 with an alpha channel
(colour type 4 or 6) stores transparency, and a reader must use
`255 − alpha` (16-bit: `65535 − alpha`). Verified on the corpus: all 22
such PNGs are mostly alpha 255 with the visible content at alpha 0
(`scope3 simple`'s 319×56 "Bitmap": 15 976 pixels `(0,0,0,255)`, the
orange swoosh at alpha 0). Palette PNGs are written with a single
transparent index and mean what they say. Preview bitmaps (60–64) use the
export path and are standard.

Corpus census, all 59 files, decoded through `xarast-image` (phase 10):
58 × tag 61 (GIF previews), 4 × tag 67 (JPEG), 32 × tag 68 (PNG, 22 with
alpha), 8 × tag 71 (JPEG8BPP); **no** tag 60, 62–66 or 69 occurs. Every one
decodes.

**`TAG_PREVIEWBITMAP_*` (60–64)** — the same but **without the name**: the payload is the
image file directly. Verified: in `testfiles/*.xar` record 61 starts with
`47 49 46 38 37 61` (`GIF87a`). The original importer simply *seeks* past it and
discards it (`Kernel/rechbmp.cpp:305-325`). It is the document thumbnail.

**`TAG_NODE_BITMAP` (198), 36 bytes** — `Kernel/cxfnbmp.cpp:118-130`:
`DocCoord p0..p3` + `i32 bitmap_ref`.
The 4 points are the corners of the parallelogram framing the image
(`NodeBitmap::Parallel[0..3]`): the distance `p0→p1` is the **width** and `p1→p2` the
**height** (`Kernel/nodebmp.cpp:468-469`); when converting it into a bitmap fill,
`p3` is used as the *start point*, `p2` as the *end point* and `p0` as *end point 2*
(`Kernel/nodebmp.cpp:1210-1212`).

**`TAG_NODE_CONTONEDBITMAP` (199), 44 bytes** — the same + `i32 start_colour_ref`
+ `i32 end_colour_ref`.

**`TAG_BITMAP_PROPERTIES` (4115), 12 bytes** — `i32 bitmap_ref`, `u8 flags`,
7 reserved bytes set to 0 (`Kernel/bmpcomp.cpp:1810-1826`).

**`TAG_DOCUMENTBITMAPSMOOTHING` (4116), 5 bytes** — `u8 flags` + 4 reserved
(`Kernel/camfiltr.cpp:6885-6895`).

**`TAG_XPE_BITMAP_PROPERTIES` (4117)**, variable: `i32 bmp_ref`, `u8 flags`, `u8 0`,
`i32 master_record`, `UNICODE-Z name`, `BSTR xml` (in XaraLX the XML is disabled:
`PORTNOTE` in `Kernel/bmpcomp.cpp:1780-1800`).

### 4.6. Category: views, units and document information

| tag | record | payload |
|---|---|---|
| 80 | `TAG_VIEWPORT` (16) | `DocCoord lo`, `DocCoord hi` (`Kernel/viewcomp.cpp:505-515`) |
| 81 | `TAG_VIEWQUALITY` (1) | `u8 quality` (`Kernel/viewcomp.cpp:565-572`) |
| 82 | `TAG_DOCUMENTVIEW` (24) | `FIXED16 scale`, `DocCoord lo`, `DocCoord hi`, `u32 flags` (`Kernel/viewcomp.cpp:678-690`) |
| 85/86 | `TAG_DEFINE_*USERUNIT` (28+strings) | `UNICODE-Z name`, `UNICODE-Z abbreviation`, `f64 unit_size`, `i32 base_unit` (REFERENCE), `f64 numerator`, `f64 denominator` (`Kernel/unitcomp.cpp:950-1000`) |
| 87 | `TAG_DEFINE_DEFAULTUNITS` (8) | `i32 page_units`, `i32 font_units` (REFERENCEs, normally negative: §5.2) |
| 90 | `TAG_DOCUMENTCOMMENT` (var) | `UNICODE-Z comment` |
| 91 | `TAG_DOCUMENTDATES` (8) | `i32 creation`, `i32 last_saved` (`time_t`-style seconds) |
| 92 | `TAG_DOCUMENTUNDOSIZE` (4) | `u32 bytes` |
| 93 | `TAG_DOCUMENTFLAGS` (4) | `u32 flags`: bit0 = all layers visible, bit1 = multi-layer (`Kernel/infocomp.cpp:744-752`) |
| 4114 | `TAG_DOCUMENTNUDGE` (4) | `u32 nudge` in millipoints |

### 4.7. Category: geometric objects

Paths (100–103, 113–116, 111, 118, 4013) → **§7**.
Regular shapes (1000–1901) → §4.7.1.

#### 4.7.1. Regular shapes

The 1000/1100/1200 families are **legacy**: Xara X and later *always* write
`TAG_REGULAR_SHAPE_PHASE_2` (1901). In the corpus: 35,206 records of 1901 and **zero** of
any other regular shape. Even so, a complete importer should support them because old
files (Camelot 1.5) use them.

**`TAG_REGULAR_SHAPE_PHASE_2` (1901)** — writing `Kernel/cxfrgshp.cpp:338-360`,
reading `Kernel/rechrshp.cpp:245-295`:

| Off | Type | Field |
|---|---|---|
| 0 | `u8` | `flags` |
| 1 | `u16` | `num_sides` |
| 3 | `DocCoord` | `major_axis` (a vector, not translated by the origin) |
| 11 | `DocCoord` | `minor_axis` |
| 19 | `Matrix` (24) | transformation matrix (this one *is* translated, §5.4) |
| 43 | `f64` | `stellation_radius` |
| 51 | `f64` | `stellation_offset` |
| 59 | `f64` | `primary_curvature` |
| 67 | `f64` | `secondary_curvature` |
| 75 | path | `primary_edge_path` (absolute format §7.2) |
| … | path | `secondary_edge_path` |

`flags` (`Kernel/cxfrgshp.cpp:481-495`): `0x01` circular (ellipse/circle),
`0x02` stellated, `0x04` primary curvature active, `0x08` stellation curvature active.
The centre is assumed to be `(0,0)` **in untransformed space**: the real position is
supplied by the matrix. (`TAG_REGULAR_SHAPE_PHASE_1` (1900) is identical but with an extra
`DocCoord ut_centre` after `num_sides`; it exists only in very old files,
`Kernel/cxftags.h:360-374`.)

**The record holds no outline.** The two edge paths are *edge templates*: each one
describes the shape of a single edge, not the shape. An unedited edge is a two-point
`MoveTo, LineTo` (a straight edge) at an arbitrary position. In the corpus it is
typically `(-576pt, -576pt) → (-504pt, -576pt)`. The outline has to be generated
from the parameters. These are the facts it depends on. They are taken from
`Kernel/nodershp.cpp`, but only as behaviour, never as code (added 2026-09-23 for
XARA-T-0013):

- **Frame.** All geometry is built in untransformed space, with the centre at the
  origin. The record's matrix is applied at the end. A point at angle `θ` and
  radius ratio `r` sits at `r·(cos θ · major − sin θ · minor)`
  (`nodershp.cpp:2402-2430` normalises onto the parallelogram
  `centre ± major ± minor`, `:2466-2486`).
- **Primary points** are at `θ_k = π/n + k·2π/n`, `k = 0…n−1`, and radius ratio 1.
- **Stellation points** (stellated only) are at `θ_k + (0.5 + stellation_offset)·2π/n`
  and radius ratio `stellation_radius` (`:2403-2406`). An offset of ±0.5 therefore
  lands on the neighbouring primary angle.
- **Outline order**: primary 0, stellation 0, primary 1, …, and then closed. An edge
  from a primary to a stellation point uses the primary edge template. An edge from a
  stellation point to a primary uses the secondary template. A polygon that is not
  stellated uses only the primary template (`:2735-2790`).
- **Edge template insertion** (`:2186-2233`): a two-point template is a straight
  line. A four-point template (one cubic) has its two control points carried by the
  **similarity** (rotation plus uniform scale) that maps the template's first point
  to the edge's start and its last point to the edge's end. When start equals end,
  the edge is a degenerate cubic.
- **Ellipse** (`circular`, `:2254-2330`): four cubics through `+major`, `+minor`,
  `−major` and `−minor`, in that order. Each control point lies on the line from
  its endpoint to the parallelogram corner between the two endpoints, at ratio
  **0.552** of that distance (`:161`).
- **Corner rounding** (`:2533-2660`). Let `L = max(1, |major| · max(1, r_s))`,
  where `r_s` is the stellation ratio for a stellated shape. The primary corners
  cut a length `L·primary_curvature` and the stellation corners cut
  `L·secondary_curvature`. On an edge of length `d` whose two corners want `a + b`:
  when `a + b > d`, both cuts are scaled by `d/(a + b)`, and when `d < 1` the cut
  collapses onto the corner. A rounded corner is one cubic from the incoming cut
  point to the outgoing one. Its controls sit on the lines from each cut point to the
  corner, at ratio 0.552. Primary corners are rounded when flag `0x04` is set, and
  stellation corners when `0x08` is set.

Typical observed size: 119 bytes (75 + 40 for a 4-point path + 4 for the empty path).

**Legacy families** (`Kernel/cxfellp.cpp:157-186`, `Kernel/cxfrect.cpp:196-470`,
`Kernel/cxfpoly.cpp:178-320`) — rules for composing the payload, in this order:

1. `u16 num_sides` — polygons only.
2. *simple*: `DocCoord centre`, `i32 width`, `i32 height`
   *complex*: `DocCoord centre`, `DocCoord major_axis`, `DocCoord minor_axis`
   *…_reformed complex*: `DocCoord ut_major`, `DocCoord ut_minor`, `Matrix` (no centre).
3. *stellated*: `f64 stellation_radius`, `f64 stellation_offset`.
4. *rounded*: `f64 curvature` (or `f64 primary` + `f64 secondary` if it is also
   stellated).
5. *reformed*: one or two absolute paths (`primary`, `secondary`).

### 4.8. Category: containers and effects

| tag | record | payload |
|---|---|---|
| 104 | `TAG_GROUP` | empty (`Kernel/group.cpp:1684`) |
| 105 | `TAG_BLEND` (3) | `u16 num_steps`, `u8 flags` (`Kernel/nodeblnd.cpp:3546-3556`). Flags (`Kernel/cxfdefs.h:352-357`): bit0 one-to-one, bit1 antialias, bit2 tangential; bits 4-7 = colour interpolation type (`(f & 0xF0) >> 4`) |
| 106 | `TAG_BLENDER` (8) | `i32 path_index_start`, `i32 path_index_end` (`Kernel/nodebldr.cpp:7188-7194`) |
| 4073 | `TAG_BLENDERADDITIONAL` (17) | `i32 blended_on_curve`, `i32 node_blend_path_index` (−1 if −2), `i32 obj_index_start`, `i32 obj_index_end`, `u8 bitfield` (bit0 = reversed) |
| 4072 | `TAG_BLENDPROFILES` (48) | 6 × `f64`: object, attribute and position bias/gain |
| 4060 | `TAG_BLENDER_CURVEPROP` (16) | `f64 prop_start`, `f64 prop_end` |
| 4062 | `TAG_BLENDER_CURVEANGLES` (16) | `f64 angle_start`, `f64 angle_end` |
| 4061 | `TAG_BLEND_PATH` (var) | absolute path (`Kernel/ndbrshpt.cpp:296-306`) |
| 4074 | `TAG_NODEBLENDPATH_FILLED` (4) | `i32 filled` |
| 107 | `TAG_MOULD_ENVELOPE` (4) | `i32 threshold` (`Kernel/nodemold.cpp:2705-2727`) |
| 108 | `TAG_MOULD_PERSPECTIVE` (4) | `i32 threshold` |
| 109 | `TAG_MOULD_GROUP` | empty |
| 110 | `TAG_MOULD_PATH` (var) | absolute path defining the mesh (`Kernel/ndmldpth.cpp:590-600`) |
| 4012 | `TAG_MOULD_BOUNDS` (16) | `DocCoord lo`, `DocCoord hi` of the original object (`Kernel/ndmldgrp.cpp:800-810`) |
| 4050 | `TAG_SHADOWCONTROLLER` (29) | `u8 shadow_type`, `i32 penumbra_width`, `i32 offset_x`, `i32 offset_y`, `i32 floor_angle` (radians encoded as an integer), `i32 floor_height` (×100), `i32 scale` (×100), `i32 feather_or_glow_width` (`Kernel/nodecont.cpp:1613-1650`) |
| 4051 | `TAG_SHADOW` (24) | `f64 bias`, `f64 gain`, `f64 darkness` (`Kernel/nodeshad.cpp:1800-1815`) |
| 4052 | `TAG_BEVEL` (24) | `i32 type`, `i32 indent`, `i32 light_angle`, `i32 is_outer`, `i32 contrast`, `i32 tilt` (`Kernel/nbevcont.cpp:500-520`). ⚠️ The size is **not** in `cxfdefs.h`; it is hard-coded in the writer |
| 4057 | `TAG_BEVELINK` | empty |
| 4053-4056 | `TAG_BEVATTR_*` (4) | `i32 value` (indent / light angle / contrast / type) (`Kernel/nodebev.cpp:3378-3400`) |
| 4066 | `TAG_CONTOURCONTROLLER` (41) | `i32 steps`, `i32 width`, `u8 type`, `f64 obj_bias`, `f64 obj_gain`, `f64 attr_bias`, `f64 attr_gain` (`Kernel/ncntrcnt.cpp:1600-1620`) |
| 4067 | `TAG_CONTOUR` | empty |
| 4084/4085 | `TAG_CLIPVIEWCONTROLLER` / `TAG_CLIPVIEW` | empty (`Kernel/nodeclip.cpp:503`) |
| 4086 | `TAG_FEATHER` (20) | `i32 size` (millipoints), `f64 bias`, `f64 gain` (`Kernel/fthrattr.cpp:2225-2240`) |
| 4125 | `TAG_LIVE_EFFECT` (var) | `u8 flags`, `f64 pixels_per_inch`, `UNICODE-Z effect_id`, `UNICODE-Z display_name`, `UTF16STR xml_edits` (`Kernel/nodeliveeffect.cpp:2648-2680`) |
| 4126/4127 | `TAG_LOCKED_EFFECT`, `TAG_FEATHER_EFFECT` | the same fields as 4125 (same writer) |
| 4128 | `TAG_COMPOUNDRENDER` (20) | `u32 reserved (0)`, `DocCoord lo`, `DocCoord hi` (`Kernel/group.cpp:1720-1735`) |
| 4129 | `TAG_OBJECTBOUNDS` (16) | `DocCoord lo`, `DocCoord hi` (`Kernel/cxftext.cpp:1035-1050`) |
| 4015 | `TAG_EXPORT_HINT` (var) | `u32 type`, `u32 width`, `u32 height`, `u32 bpp`, `ASCII-Z options` (`Kernel/exphint.cpp:230-245`) |
| 4020 | `TAG_WEBADDRESS` (var) | `UNICODE-Z url`, `UNICODE-Z frame` (`Kernel/rechattr.cpp:300-330`) |
| 4021 | `TAG_WEBADDRESS_BOUNDINGBOX` (var) | `DocCoord lo` **interleaved**, `DocCoord hi` **interleaved**, `UNICODE-Z url`, `UNICODE-Z frame` (`Kernel/rechattr.cpp:355-380`) |
| 189 | `TAG_USERVALUE` (var) | `UNICODE-Z key`, `UNICODE-Z value` |
| 4040 | `TAG_WIZOP` (var) | 4 × `UNICODE-Z`: internal name, question, parameter, empty string (`Kernel/tmpltatr.cpp:395-415`) |
| 4042 | `TAG_WIZOP_STYLEREF` (4) | `i32 reference to the style` |

**ClipView structure** (XARA-T-0306). Both records are empty; the meaning is
in the order of the controller's children. The ink children **before** the
`TAG_CLIPVIEW` child are the *keyholes*, the ones **after** it are clipped
(`Kernel/ndclpcnt.h:123-133`: `Keyhole — NCV — Clipped Node N1 — N2 …`;
`Kernel/ndclpcnt.cpp:328-339` walks left of the `NodeClipView` for keyholes).
Only the first `TAG_CLIPVIEW` child counts (`GetClipView`,
`Kernel/ndclpcnt.cpp:288-296`). The keyholes are the bottom-most objects of
the group and are **drawn as ordinary objects** — the controller renders as a
plain group (`Kernel/ndclpcnt.cpp:513-527`) — and the clip region is the
**union** of their filled areas with outlines stripped (`UpdateKeyholePath`,
`Kernel/ndclpcnt.cpp:1938-2000`: `PathBecomeA … STRIP_OUTLINES`, combined with
`ClipPathToPath` style 7, which is `Source OR Clip`, `Kernel/paths.cpp:5461-5468`).
An empty union clips everything away (a degenerate keyhole path, `:1996-2007`).
Xara LX declares both tags atomic, so a reader that does not know them drops
the whole controller. `TAG_CLIPVIEW_PATH` (4137) is declared and never
written.

### 4.9. Category: printing and imagesetting

| tag | record | payload |
|---|---|---|
| 3500-3505 | `TAG_OVERPRINT*`, `TAG_PRINTONALLPLATES*` | empty (boolean attributes) |
| 3506 | `TAG_PRINTERSETTINGS` (45) | 45 one-byte fields and integers, written with macros (`Kernel/princomp.cpp:700-780`). Of little interest to a graphics importer |
| 3507 | `TAG_IMAGESETTING` (15) | `i32 print_resolution`, `f64 screen_frequency`, `u16 screen_function`, `u8 flags` (`Kernel/princomp.cpp:880-895`) |
| 3508 | `TAG_COLOURPLATE` (22) | `u8 type`, `i32 spot_colour_ref`, `f64 screen_angle`, `f64 screen_frequency`, `u8 flags` (`Kernel/princomp.cpp:960-985`) |
| 3509 | `TAG_PRINTMARKDEFAULT` (1) | `u8 id` of the mark (`Kernel/prnmkcom.cpp:710-720`) |
| 3510 | `TAG_PRINTMARKCUSTOM` (var) | custom mark; its content is a **subtree** of objects |

### 4.10. Category: brushes and strokes

All of these are *rare* (only `testfiles/Brush Test.xar` uses them in the corpus).

| tag | record | payload, summarised |
|---|---|---|
| 4002 | `TAG_STROKETYPE` (4) | `u32 handle` of the stroke definition (`Kernel/strkattr.cpp:540-555`) |
| 4000 | `TAG_VARIABLEWIDTHFUNC` (4) | `u32 0`, `u32 function_id` — ⚠️ the writer emits **8** bytes even though `cxfdefs.h` declares 4 (`Kernel/strkattr.cpp:1340-1350`) |
| 4001 | `TAG_VARIABLEWIDTHTABLE` (var) | table of widths |
| 4003 | `TAG_STROKEDEFINITION` (var) | `u32 handle`, `u32 flags`, `u32 num_repeats`, `u32 0` (`Kernel/strkcomp.cpp:865-880`) |
| 4079 | `TAG_BRUSHATTR` (33) | `u32 brush_handle`, `i32 spacing`, `u8 flags`, `f64 rotate_angle`, `i32 path_offset_type`, `i32 path_offset_value`, `f64 scaling` (`Kernel/ppbrush.cpp:7355-7375`) |
| 4080 | `TAG_BRUSHDEFINITION` (4) | `u32 handle` (`Kernel/brshcomp.cpp:3350-3360`) |
| 4082/4083 | `TAG_MOREBRUSHDATA` (68) / `TAG_MOREBRUSHATTR` (72) | 11×`i32` + 3×`f64` (+ an extra `i32` in ATTR) |
| 4102/4103 | `TAG_EVENMOREBRUSHDATA` (8) / `…ATTR` (9) | 2×`i32` (+ `u8`) |
| 4105/4107 | `TAG_BRUSHPRESSUREINFO` / `…ATTRPRESSUREINFO` (28) | 7×`i32` |
| 4112/4113 | `TAG_BRUSHTRANSPINFO` / `…ATTRTRANSPINFO` (40) | 6×`i32` + 2×`f64` |
| 4111 | `TAG_BRUSHATTRFILLFLAGS` (1) | `u8` with `0x1` local fill, `0x2` local transp, `0x4` named colour (`Kernel/cxfdefs.h:585-588`) |

### 4.11. Tags declared but **not implemented** in Xara LX

The following 21 tags are defined in `Kernel/cxftags.h` but have **neither a writer nor a
reader** anywhere in the code tree (exhaustive search for references outside the tables in
`cxftags.h`, `cxfdefs.h`, `cxfrech.cpp`, `cxftree.cpp`, `cxfmap.cpp`).
**Their layout is unknown** and they must be treated as an "unknown tag" (§2.3):

```
20  TAG_NONRENDERSECTION_START      21  TAG_NONRENDERSECTION_END
22  TAG_RENDERING_PAUSE             23  TAG_RENDERING_RESUME
95  TAG_NAMEGAL_DOCCOMP            187  TAG_DEFINEARROW
4014 TAG_PATHREF_TRANSLATE         4063 TAG_GROUPTRANSP
4064 TAG_CACHEBMP                  4065 TAG_CACHEDNODESGROUP
4108 TAG_BRUSHCOLOURDATA           4110 TAG_BRUSHTIMESAMPLEDATA
4130 TAG_DIMENSION                 4134 TAG_SPREAD_FLASHPROPS
4135 TAG_PRINTERSETTINGS_PHASE2    4136 TAG_DOCUMENTINFORMATION
4137 TAG_CLIPVIEW_PATH             4138 TAG_DEFINEBITMAP_PNG_REAL
4205 TAG_TEXT_STORY_HEIGHT_INFO    4206 TAG_TEXT_STORY_LINK_INFO
4207 TAG_TEXT_STORY_TRANSLATION_INFO
```

In addition, `TAG_SPREAD_PHASE2` (4131) and `TAG_CURRENTATTRIBUTES_PHASE2` (4132) appear
only **commented out** in `Kernel/camfiltr.cpp:7014-7015`: they are reserved for later
versions of Xara (the commercial Xara Xtreme Pro) and **may appear in modern files**
without this code knowing how to read them (§11).

`TAG_DEFINEARROW` (187) deserves a separate mention: the format anticipates user-defined
custom arrowheads, but Xara LX only knows how to write/read the 8 predefined arrowheads by
negative reference (§8.6).

### 4.12. Category: text

References: `Kernel/rechtext.cpp` (reading of all text records),
`Kernel/cxftext.cpp` (writing), `Kernel/cxfdefs.h:445-500` (sizes).

#### 4.12.1. Font definitions (referenceable)

**`TAG_FONT_DEF_TRUETYPE` (2000)** and **`TAG_FONT_DEF_ATM` (2001)**, variable size
(`Kernel/fontcomp.cpp:893-930`):

| Type | Field |
|---|---|
| `UNICODE-Z` | full name of the font (e.g. `"Arial"`) |
| `UNICODE-Z` | name of the *typeface* |
| `CCPanose` (10 bytes) | PANOSE number |

Verified: `TextDesigns/SimpleText.xar` → size 34 = 12 + 12 + 10.
**Glyphs are not embedded**: the file only stores the identification of the font
(the corpus filename `embeddedFonts.xar` is misleading; it still uses these records).

#### 4.12.2. Text stories (root nodes of a text block)

| tag | record | payload |
|---|---|---|
| 2100 | `TAG_TEXT_STORY_SIMPLE` (12) | `DocCoord anchor`, `i32 autokern` |
| 2101 | `TAG_TEXT_STORY_COMPLEX` (28) | `Matrix` (24), `i32 autokern` |
| 2110-2113 | `TAG_TEXT_STORY_SIMPLE_{START,END}_{LEFT,RIGHT}` (12) | `DocCoord`, `i32 autokern` — text on a path: the name says from which end and in which direction it flows |
| 2114-2117 | `TAG_TEXT_STORY_COMPLEX_{START,END}_{LEFT,RIGHT}` (36) | `Matrix` (24), `ANGLE rotation`, `ANGLE shear`, `i32 autokern` |

The `autokern` field is read with `ReadINT32noError` (`Kernel/rechtext.cpp:756-770`): if
the record is of the old version (4 bytes shorter) it is simply absent. **The Rust reader
must do the same.**

#### 4.12.3. Internal structure of a story

```
TAG_TEXT_STORY_*            (story node)
  TAG_DOWN
    TAG_TEXT_STORY_WORD_WRAP_INFO   (i32 width, u8 word_wrap_on)      [5 bytes]
    TAG_TEXT_STORY_INDENT_INFO      (i32 left_indent, i32 right_indent)[8 bytes]
    TAG_TEXT_LINE                   (empty)  <- one per line
      TAG_DOWN
        TAG_TEXT_LINE_INFO          (i32 width, i32 height, i32 dist_prev) [12]
        TAG_TEXT_STRING             (UTF-16 with no NUL)  <- plain text
        TAG_TEXT_CHAR               (u16)             <- single character with its own attributes
        TAG_TEXT_KERN               (i32 dx, i32 dy)
        TAG_TEXT_TAB                (empty)
        TAG_TEXT_EOL                (empty)
        ... text attributes (2900-2920, 4201-4204) ...
      TAG_UP
  TAG_UP
```

`TAG_TEXT_STRING` (2201) is imported in a peculiar way: the reader inserts **only the
first character** as a node, and expands the rest once the subtree is finished, copying
the child attributes of the first character (`Kernel/rechtext.cpp:620-662`). For a new
importer this is irrelevant except for one semantic consequence: **the attributes hanging
off a `TAG_TEXT_STRING` apply to the whole string**.

#### 4.12.4. Text attributes

| tag | record | payload |
|---|---|---|
| 2900 | `TAG_TEXT_LINESPACE_RATIO` (4) | `FIXED16 ratio` |
| 2901 | `TAG_TEXT_LINESPACE_ABSOLUTE` (4) | `i32 millipoints` |
| 2902-2905 | `TAG_TEXT_JUSTIFICATION_{LEFT,CENTRE,RIGHT,FULL}` (0) | empty |
| 2906 | `TAG_TEXT_FONT_SIZE` (4) | `i32 millipoints` (10 pt → 10000) |
| 2907 | `TAG_TEXT_FONT_TYPEFACE` (4) | `i32` **reference** to the `TAG_FONT_DEF_*` record |
| 2908/2909 | `TAG_TEXT_BOLD_ON/OFF` (0) | empty |
| 2910/2911 | `TAG_TEXT_ITALIC_ON/OFF` (0) | empty |
| 2912/2913 | `TAG_TEXT_UNDERLINE_ON/OFF` (0) | empty |
| 2914 | `TAG_TEXT_SCRIPT_ON` (8) | `FIXED16 offset`, `FIXED16 size` |
| 2915 | `TAG_TEXT_SCRIPT_OFF` (0) | empty |
| 2916/2917 | `TAG_TEXT_SUPERSCRIPT_ON` / `SUBSCRIPT_ON` (0) | empty (implicit values) |
| 2918 | `TAG_TEXT_TRACKING` (4) | `i32` — the internal type is `MILLIPOINT` (`Kernel/txtattr.h:413`), but it is scaled by the font size when applied; **the exact unit is ambiguous, verify against the render** (§11) |
| 2919 | `TAG_TEXT_ASPECT_RATIO` (4) | `FIXED16` |
| 2920 | `TAG_TEXT_BASELINE` (4) | `i32 millipoints` |
| 4201/4202/4203 | `TAG_TEXT_{LEFT,FIRST,RIGHT}_INDENT` (4) | `i32 millipoints` |
| 4204 | `TAG_TEXT_RULER` (var) | `u16 num_entries`; per entry: `u8 type_and_flags`, `i32 position`, and —if the type says so— `u16 decimal_char` and/or `u16 tab_filler_char` (`Kernel/rechtext.cpp:1547-1580`) |

---

## 5. Coordinate system and units

### 5.1. Millipoints

Xara's internal unit is the **millipoint** = 1/1000 of a PostScript point
(`Kernel/units.h:115-132`; `Kernel/rechdoc.cpp:598` uses the `MILLIPOINT` type).

| Unit | millipoints | constant |
|---|---:|---|
| 1 millipoint | 1 | `MP_MP_VAL` |
| 1 point (pt) | 1 000 | `PT_MP_VAL` |
| 1 pica | 12 000 | `PI_MP_VAL` |
| 1 inch | 72 000 | `IN_MP_VAL` |
| 1 foot | 864 000 | `FT_MP_VAL` |
| 1 yard | 2 592 000 | `YD_MP_VAL` |
| 1 mile | 4 561 920 000 | `MI_MP_VAL` |
| 1 millimetre | 2 834.652715 | `MM_MP_VAL` |
| 1 centimetre | 28 346.52715 | `CM_MP_VAL` |
| 1 metre | 2 834 652.715 | `M_MP_VAL` |
| 1 kilometre | 2 834 652 715 | `KM_MP_VAL` |
| 1 pixel (at 96 dpi) | 750 | `PX_MP_VAL` |

Conversions useful to the importer:

```rust
pub const MP_PER_PT: f64 = 1000.0;
pub const MP_PER_IN: f64 = 72_000.0;
pub const MP_PER_MM: f64 = 2834.652715;   // = 72000 / 25.4
pub const MP_PER_PX96: f64 = 750.0;

#[inline] pub fn mp_to_pt(v: i32) -> f64 { v as f64 / MP_PER_PT }
#[inline] pub fn mp_to_mm(v: i32) -> f64 { v as f64 / MP_PER_MM }
#[inline] pub fn mp_to_px(v: i32, dpi: f64) -> f64 { v as f64 * dpi / MP_PER_IN }
```

### 5.2. Precision and range

Coordinates are signed `i32` → a range of ±2 147 483 647 millipoints ≈ ±2 147 483 pt
≈ ±29 826 inches ≈ ±757 metres. The resolution is 1/1000 pt ≈ 0.35 µm.
**An importer must use `i32` (not `f32`) for native coordinates** and convert to floating
point only at the end: `f32` loses precision above ~16.7 million millipoints (≈ 233 pt of
error in the worst case for a large document).

Predefined units referenced by a negative number (`Kernel/cxfunits.h:104-116`):

| value | unit |
|---:|---|
| −1 | untyped |
| −2 | millimetres |
| −3 | centimetres |
| −4 | metres |
| −5 | kilometres |
| −6 | millipoints |
| −7 | computer points (pt) |
| −8 | picas |
| −9 | inches |
| −10 | feet |
| −11 | yards |
| −12 | miles |
| −13 | pixels |

(`testfiles/OneLine.xar` → `TAG_DEFINE_DEFAULTUNITS` = `F3 FF FF FF F9 FF FF FF` =
−13 (pixels) for the page and −7 (points) for the font.)

### 5.3. Origin and direction of the Y axis

* **Y axis pointing up.** The system is classically Cartesian: `Spread::SetPageSize`
  places "the **bottom** left corner of the bottom-left page" at the origin
  (`Kernel/spread.cpp:1843-1850`), and `DocRect` has `lo` = bottom-left corner and
  `hi` = top-right.
* **Coordinate origin of the records: the `lo` corner of the rectangle enclosing all
  the pages of the spread.** On export that origin is subtracted from every coordinate and
  on import it is added back (`Kernel/cxfrec.cpp:2080-2135` and `2225-2280`,
  `CamelotFileRecord::Write/ReadCoord` with `CoordOrigin`).
  The origin is set in `BaseCamelotFilter::SetCoordOrigin()` (`Kernel/camfiltr.cpp:5539`),
  and during import it is updated when `TAG_SPREADINFORMATION` is processed
  (`Kernel/rechdoc.cpp:640-644`).
* **How to compute that corner** — this is what an importer actually needs, and the
  paragraph above does not say it. A spread lays its first page out with its `lo`
  corner at `(PageMargin, PageMargin)` in spread coordinates, where

  ```
  PageMargin = if Margin < Bleed { Margin + Bleed } else { Margin }
  ```

  with `Margin` and `Bleed` the two fields of `TAG_SPREADINFORMATION`
  (`Spread::SetSizeOfAllPages`, `Kernel/spread.cpp:2506-2530`, called from
  `Spread::SetPageSize`, `:2196-2290`). A double page spread puts the second page one
  page-width to the right of the first, so the union's `lo` corner is the first page's
  either way. Therefore

  ```
  CoordOrigin = (PageMargin, PageMargin)
  ```

  **It is not `(0,0)` in a normal file.** `Margin` is 576 000 or 566 931 millipoints in
  57 of the 59 corpus files and 0 only in `Templates/animation.xar`. Two records make
  the value checkable without any rendering: an empty document writes `TAG_VIEWPORT`
  (the drawing's bounding box, written with the origin subtracted) as the degenerate
  rectangle `(-Margin, -Margin, -Margin, -Margin)`, and every
  `TAG_CURRENTATTRIBUTEBOUNDS` in the corpus is `(-Margin, -Margin)` — both an empty
  `DocRect` at spread coordinate `(0,0)`. `animation.xar`, whose margin is 0, writes
  `(0,0,0,0)` for the same records.
* **Practical consequence**: file coordinates are page coordinates with Y pointing up
  and `(0,0)` at the bottom-left corner of the page; the origin translation places that
  page inside the spread's pasteboard, whose own `lo` corner is `(0,0)`.
  In `testfiles/OneLine.xar` the page is 600 000 × 450 000 mp with a 576 000 mp margin,
  and the path at file coordinates (112 101, 178 899) → (283 101, 321 399) sits at spread
  coordinates (688 101, 754 899) → (859 101, 897 399): inside the page either way, which
  is why this file alone does not distinguish the two.
* **When exporting to SVG/PDF/any system with Y pointing down**, apply
  `y' = page_height - y`.

⚠️ **An important nuance for implementers:** only coordinates read via
`ReadCoord`, `ReadCoordInterleaved`, `ReadPath`, `ReadPathRelative`, `ReadMatrix`,
`ReadXOrd` and `ReadYOrd` receive the origin translation. **Vectors** (the major and minor
axes of regular shapes) are written with `WriteCoordTrans(...,0,0)`, that is **without
translation** (`Kernel/cxfrgshp.cpp:347-348`). When implementing, that distinction must be
respected: points → translated; vectors → not.

⚠️ **`TAG_VIEWPORT` (80) is the exception that does not round trip.** It is *written*
with the origin subtracted (`CamelotFileRecord::WriteCoord`, `Kernel/viewcomp.cpp:512-515`)
and *read* without the translation (`ReadCoordTrans(...,0,0)`,
`Kernel/viewcomp.cpp:849-851`). The original is inconsistent here and only uses the record
for the minimal-web format and for import-at-position, so nothing depends on it; read it
untranslated, as the handler does. `TAG_DOCUMENTVIEW` (82) uses plain `ReadCoord` and *is*
translated.

### 5.4. Matrices

Format (24 bytes, `Kernel/cxfrec.cpp:1913-1959`):

```
FIXED16 a   FIXED16 b   FIXED16 c   FIXED16 d   INT32 e   INT32 f
```

which corresponds to the affine transformation

```
| x' |   | a  c |   | x |   | e |
|    | = |      | * |   | + |   |
| y' |   | b  d |   | y |   | f |
```

* `a, b, c, d` are **FIXED16** (÷65536) → a precision of ≈ 1.5 × 10⁻⁵ in the scale and
  rotation factors. **This limits the fidelity of rotations**: an angle is represented
  with at most ~4.5 arcseconds of error.
* `e, f` are millipoints and they **do** carry the coordinate origin translation
  (`ReadMatrixTrans` adds `CoordOrigin`).

```rust
#[derive(Clone, Copy, Debug)]
pub struct Matrix { pub a: f64, pub b: f64, pub c: f64, pub d: f64, pub e: i32, pub f: i32 }

impl Matrix {
    pub fn apply(&self, p: Coord) -> Coord {
        Coord {
            x: (self.a * p.x as f64 + self.c * p.y as f64).round() as i32 + self.e,
            y: (self.b * p.x as f64 + self.d * p.y as f64).round() as i32 + self.f,
        }
    }
}
```

---

## 6. Tree model, definitions and references

### 6.1. Building the tree with DOWN/UP

The file is a pre-order traversal of the document's node tree:

* An **object** record creates a node and inserts it as the **next sibling** of the last
  node inserted at the current level (`BaseCamelotFilter::InsertNode`,
  `Kernel/camfiltr.cpp:5999-6040`).
* `TAG_DOWN` (1) pushes a level: the next node inserted will be a **child** of the last
  node (`IncInsertLevel`, `Kernel/camfiltr.cpp:6507-6532`).
* `TAG_UP` (0) pops: it returns to the previous level and notifies the parent node that
  its subtree is complete (`DecInsertLevel`, `Kernel/camfiltr.cpp:6556-6590`; this is
  where composite nodes —moulds, blends, text— do their deferred initialisation,
  `ReadPostChildren*`).

Canonical hierarchy of a document:

```
TAG_FILEHEADER
TAG_PREVIEWBITMAP_GIF                 (optional, uncompressed)
TAG_DOCUMENT
  DOWN
    TAG_DOCUMENTNUDGE, TAG_DOCUMENTBITMAPSMOOTHING, ...
    TAG_STARTCOMPRESSION
    TAG_VIEWPORT, TAG_ATOMICTAGS...
    TAG_CHAPTER
      DOWN
        TAG_SPREAD
          DOWN
            TAG_SPREADINFORMATION, TAG_SPREADSCALING_*, TAG_SPREAD_ANIMPROPS
            TAG_LAYER
              DOWN
                TAG_LAYERDETAILS
                <objects of the drawing>
                  DOWN  <attributes and children of the object>  UP
              UP
            TAG_GRIDRULERSETTINGS, TAG_GRIDRULERORIGIN
          UP
      UP
    TAG_SETSENTINEL, TAG_BARPROPERTY, units, document info, views
    TAG_ENDCOMPRESSION
  UP
TAG_ENDOFFILE
```

(Extracted from `testfiles/OneLine.xar`; maximum depth observed in the corpus: 13 levels,
in `testfiles/ProbeX16.xar`.)

Special rules observed in the code:

* A **layer** inserted when the context node is already a layer is added as a *sibling*,
  not as a child (`Kernel/camfiltr.cpp:6006-6020`).
* The **attributes** of an object are written as **children** of that object
  (`NodePath::WriteBeginChildRecordsNative`, `Kernel/nodepath.cpp:2640-2683`: it emits
  `TAG_DOWN`, then `TAG_PATH_FLAGS`, then the attributes, then `TAG_UP`).

### 6.2. Pseudocode for building the tree

```rust
pub struct Node { pub tag: u32, pub rec: u32, pub payload: Vec<u8>, pub children: Vec<Node> }

pub fn build_tree(records: impl Iterator<Item = Record>) -> Vec<Node> {
    let mut roots: Vec<Node> = Vec::new();
    let mut stack: Vec<Vec<Node>> = Vec::new();  // open levels
    let mut cur: Vec<Node> = Vec::new();         // siblings at the current level

    for r in records {
        match r.tag {
            TAG_DOWN => { stack.push(std::mem::take(&mut cur)); }
            TAG_UP => {
                let finished = std::mem::replace(&mut cur, stack.pop().unwrap_or_default());
                if let Some(parent) = cur.last_mut() { parent.children = finished; }
                else { roots.extend(finished); }   // unbalanced DOWN/UP: tolerate it
            }
            TAG_ENDOFFILE => break,
            _ => cur.push(Node { tag: r.tag, rec: r.number, payload: r.data, children: vec![] }),
        }
    }
    while let Some(prev) = stack.pop() {           // implicit close at the end
        let finished = std::mem::replace(&mut cur, prev);
        if let Some(parent) = cur.last_mut() { parent.children = finished; }
    }
    roots.extend(cur);
    roots
}
```

> **Note:** in the corpus the number of `TAG_DOWN` and `TAG_UP` records matches exactly
> (201 121 of each), but the parser must tolerate imbalances (truncated files).

### 6.3. Definitions and references by record number

**The record number is the universal reference key.** It is counted from 1, counting
*every* record in the file (including `TAG_UP`/`TAG_DOWN`, the header and the compressed
ones), in order of appearance:

* Writing: `RecordNumber++` in `CXaraFile::Write` (`Kernel/cxfile.cpp:1650`),
  `WriteRecordHeader` (`Kernel/cxfile.cpp:1709`) and `StartStreamedRecord`
  (`Kernel/cxfile.cpp:1458`).
* Reading: `RecordNumber++` in `ReadNextRecordHeader` (`Kernel/cxfile.cpp:1863`).
* `WriteDefinitionRecord()` is identical to `Write()` at the binary level
  (`Kernel/cxfile.cpp:1673-1677`); the distinction is purely semantic.

Record types that act as a **referenceable definition**:

| definition | tags | referenced from |
|---|---|---|
| Colour | 50, 51 | `TAG_FLATFILL`, `TAG_LINECOLOUR`, gradients, `TAG_NODE_CONTONEDBITMAP`, `TAG_COLOURPLATE`, parent colours (tints/links) |
| Bitmap | 65–71 | `TAG_BITMAPFILL`, `TAG_CONTONEBITMAPFILL`, `TAG_BITMAPTRANSPARENTFILL`, `TAG_NODE_BITMAP`, `TAG_BITMAP_PROPERTIES` |
| Font | 2000, 2001 | `TAG_TEXT_FONT_TYPEFACE` |
| Path | 100–103, 113–116 | `TAG_PATHREF_TRANSFORM`, `TAG_PATHREF_IDENTICAL` |
| Unit | 85, 86 | `TAG_DEFINE_DEFAULTUNITS`, `TAG_GRIDRULERSETTINGS`, `TAG_SPREADSCALING_*` |
| Style (WizOp) | 4041 | `TAG_WIZOP_STYLEREF` |
| Stroke / brush | 4003, 4080 | `TAG_STROKETYPE`, `TAG_BRUSHATTR` (by *handle*, not by record number) |

**Negative references = built-in predefined objects**, they are not record numbers:

* Colours (`Kernel/cxfcols.h:104-113`): −1 transparent/none, −2 black, −3 white,
  −4 red, −5 green, −6 blue, −7 cyan, −8 magenta, −9 yellow, −10 key (CMYK black).
* Bitmap (`Kernel/cxfdefs.h:158`): −1 default bitmap.
* Arrowheads (`Kernel/cxfarrow.h:104-112`): −1 none, −2 straight, −3 angled, −4 rounded,
  −5 dot, −6 diamond, −7 feather, −8 feather 2, −9 hollow diamond.
* Dashes (`Kernel/cxfdash.h:104-126`): −1..−20 patterns 1..20, −21 solid,
  −22 guide-layer pattern.
* Units: see §5.2.

A reference value of **0 means error / none** (the writer returns 0 on failure,
`Kernel/cxfile.cpp:1654`).

```rust
#[derive(Copy, Clone, Debug)]
pub enum Ref { None, Builtin(i32), Record(u32) }

impl Ref {
    pub fn parse(v: i32) -> Ref {
        match v { 0 => Ref::None, n if n < 0 => Ref::Builtin(n), n => Ref::Record(n as u32) }
    }
}
```

**Design implication for the Rust importer:** either do **two passes** or keep a
`HashMap<u32, Definition>` that is filled as the records are read (definitions are always
written **before** their first use, because the exporter emits them on the fly:
`Kernel/fillattr.cpp:5933` writes the colour just before the fill that uses it). A single
pass with an incremental map is sufficient, and it is what the original code does.

---

## 7. Path representation

### 7.1. Path tags and their semantics

| tag | format | fill | stroke |
|---|---|---|---|
| 100 `TAG_PATH` | absolute | no | no |
| 101 `TAG_PATH_FILLED` | absolute | yes | no |
| 102 `TAG_PATH_STROKED` | absolute | no | yes |
| 103 `TAG_PATH_FILLED_STROKED` | absolute | yes | yes |
| 113 `TAG_PATH_RELATIVE` | relative | no | no |
| 114 `TAG_PATH_RELATIVE_FILLED` | relative | yes | no |
| 115 `TAG_PATH_RELATIVE_STROKED` | relative | no | yes |
| 116 `TAG_PATH_RELATIVE_FILLED_STROKED` | relative | yes | yes |

Tag selection: `NodePath::ChooseTagValue`, `Kernel/nodepath.cpp:2372-2400`.
In the corpus **only the relative ones appear** (115 and 116): Xara X always exports in
relative format (`BaseCamelotFilter::WritePathsInRelativeFormat()`,
`Kernel/camfiltr.h:251`). The absolute ones still appear *embedded* inside other records
(reformed regular shapes, `TAG_MOULD_PATH`, `TAG_BLEND_PATH`), which always use the
absolute format.

The "fill/stroke" flags in the tag **do not replace the attributes**: they say whether the
path should be filled and/or stroked with the inherited attributes (§8).

### 7.2. Absolute format (`WritePath` / `ReadPath`, `Kernel/cxfrec.cpp:1663-1745`)

```
i32  num_coords
u8   verb[num_coords]           // one per point
i32  x0, i32 y0                 // num_coords pairs of absolute coordinates
i32  x1, i32 y1
...
```

Size = `4 + num_coords * 9`.

### 7.3. Interleaved relative format (`Kernel/cxfrec.cpp:1749-1900`)

⚠️ **There is no point count.** The reader deduces it:
`num_coords = record_size / 9` (`Kernel/cxfrec.cpp:1837`).

The `RELPATHINTERLEAVE` macro **is defined** (`Kernel/cxfrec.cpp:134`), so the active
branch is the interleaved one:

```
for each point i:
    u8  verb
    u8  b0..b7   // 8 bytes with the bytes of X and Y interleaved (MSB first):
                 //   b0 = (X >> 24) & 0xFF   b1 = (Y >> 24) & 0xFF
                 //   b2 = (X >> 16) & 0xFF   b3 = (Y >> 16) & 0xFF
                 //   b4 = (X >>  8) & 0xFF   b5 = (Y >>  8) & 0xFF
                 //   b6 = (X >>  0) & 0xFF   b7 = (Y >>  0) & 0xFF
```

where the value stored is:

* for **point 0**: the **absolute** coordinate (already translated by `CoordOrigin`);
* for point `i > 0`: the **inverted delta** `D = P[i-1] - P[i]`, so that
  **`P[i] = P[i-1] - D`** (`Kernel/cxfrec.cpp:1885-1889`).

> Mind the sign: it is **not** `P[i] = P[i-1] + D`. The writer computes
> `RelX = pCoord[i-1].x - pCoord[i].x` (`Kernel/cxfrec.cpp:1812`).

**Verification with `testfiles/OneLine.xar`** (record 115, 18 bytes):

```
06 | 00 00 01 02 B5 BA E5 D3 | 02 | FF FF FD FD 64 D3 08 5C
 ^verb MOVETO   X=0x0001B5E5=112101  Y=0x0002BAD3=178899
                ^verb LINETO  D=(0xFFFD6408, 0xFFFDD35C) = (-171000, -142500)
                -> P1 = (112101+171000, 178899+142500) = (283101, 321399)
```

The interleaving exists so that the high bytes (almost always identical, 0x00 or 0xFF) end
up contiguous and zlib compresses them better.

### 7.4. Verbs (`PathVerb`)

`GDraw/gconsts.h:108-112`:

| Name in the original | Value | Role |
|---|---|---|
| `PT_CLOSEFIGURE` | `0x01` | bit-flag: this point **closes** the subpath |
| `PT_LINETO` | `0x02` | type: straight segment |
| `PT_BEZIERTO` | `0x04` | type: cubic Bézier segment |
| `PT_MOVETO` | `0x06` | type: start of a subpath (`PT_LINETO | PT_BEZIERTO`) |
| `PT_PATHELEMENT` | `0x06` | mask used to extract the type |

Bits 3–7 are unused; the verb byte carries only the type (bits 1–2) and the close flag
(bit 0).

Decoding:

```
type = verb & 0x06     // PT_PATHELEMENT
  2 -> LineTo
  4 -> BezierTo   (points come in threes: c1, c2, endpoint)
  6 -> MoveTo
closed = verb & 0x01   // PT_CLOSEFIGURE: this point CLOSES the subpath
```

Observations:

* **Béziers are cubic** and occupy **three consecutive points** with the `PT_BEZIERTO`
  verb: the first two are the control points and the third is the endpoint
  (`Kernel/nodepath.cpp:1307`, `Kernel/gclips.h:232-238`).
* `PT_CLOSEFIGURE` is a **bit** combined with the verb of the **last point** of the
  subpath (e.g. `0x03` = LineTo that closes, `0x05` = BezierTo that closes).
* The value 0 is not a valid verb.

### 7.5. `TAG_PATH_FLAGS` (111): per-point flags

Writing: `Kernel/nodepath.cpp:2645-2680`. It is emitted as the **first child** of the path
record (inside its `TAG_DOWN`). Payload: **one byte per point**, in the same order as the
points of the path; `size` = number of points.

`Kernel/cxfdefs.h:313-315`:

| bit | value | name | meaning |
|---|---|---|---|
| 0 | 0x01 | `TAG_PATH_FLAGS_SMOOTH` | the point is smooth (collinear tangents) |
| 1 | 0x02 | `TAG_PATH_FLAGS_ROTATE` | the point is a rotate point (keeps the angle, not the length) |
| 2 | 0x04 | `TAG_PATH_FLAGS_ENDPOINT` | the point is an endpoint (not a control point) |

These flags are **editing metadata**: they do not affect the rendered geometry (the
geometry is already in the coordinates). A pure importer may ignore them; an editor must
preserve them.

### 7.6. References between paths

* **`TAG_PATHREF_TRANSFORM` (118), 28 bytes** (`Kernel/nodepath.cpp:2537-2560`):
  `i32 src_path_record`, `Matrix` (24). This node's path is that of the referenced record
  transformed by the matrix. It is used to deduplicate repeated shapes.
* **`TAG_PATHREF_IDENTICAL` (4013), 4 bytes**: `i32 src_path_record`.
* **`TAG_PATHREF_TRANSLATE` (4014)**: declared, **not implemented** (§4.11).

Neither appears in the corpus (Xara only emits them if the filter defines a tolerance for
similar paths; the native filter returns a tolerance of 0,
`Kernel/native.cpp:462-466`).

### 7.7. Rust pseudocode

```rust
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Verb { MoveTo, LineTo, CurveTo }

#[derive(Clone, Debug)]
pub struct PathPoint { pub verb: Verb, pub close: bool, pub p: Coord, pub flags: u8 }

/// Absolute paths: TAG_PATH..TAG_PATH_FILLED_STROKED and embedded paths.
pub fn read_path_absolute(c: &mut Cur, origin: Coord) -> Result<Vec<PathPoint>, Err> {
    let n = c.i32()?;
    if n < 0 { return Ok(vec![]); }
    let n = n as usize;
    let verbs: Vec<u8> = (0..n).map(|_| c.u8()).collect::<Result<_, _>>()?;
    let mut out = Vec::with_capacity(n);
    for v in verbs {
        let p = c.coord()?;
        out.push(mk_point(v, Coord { x: p.x + origin.x, y: p.y + origin.y }));
    }
    Ok(out)
}

/// Interleaved relative paths: TAG_PATH_RELATIVE*. `size` = length of the payload.
pub fn read_path_relative(c: &mut Cur, origin: Coord) -> Result<Vec<PathPoint>, Err> {
    let n = c.remaining() / 9;          // there is no count!
    let mut out: Vec<PathPoint> = Vec::with_capacity(n);
    for i in 0..n {
        let v = c.u8()?;
        let d = c.coord_interleaved()?;
        let p = if i == 0 {
            Coord { x: d.x + origin.x, y: d.y + origin.y }   // absolute + origin
        } else {
            let prev = out[i - 1].p;
            Coord { x: prev.x - d.x, y: prev.y - d.y }        // CAREFUL: SUBTRACT
        };
        out.push(mk_point(v, p));
    }
    Ok(out)
}

fn mk_point(v: u8, p: Coord) -> PathPoint {
    let verb = match v & 0x06 { 2 => Verb::LineTo, 4 => Verb::CurveTo, _ => Verb::MoveTo };
    PathPoint { verb, close: v & 0x01 != 0, p, flags: 0 }
}

/// Applies the TAG_PATH_FLAGS record (a child of the path) to the points already read.
pub fn apply_path_flags(pts: &mut [PathPoint], data: &[u8]) {
    for (pt, f) in pts.iter_mut().zip(data.iter()) { pt.flags = *f; }
}
```

---

## 8. Attributes and their inheritance model

### 8.1. Conceptual model

Xara does **not** have an attribute stack with explicit push/pop in the file. The model
is:

* Attributes are **child nodes** of the object they apply to. They are emitted inside the
  object's `TAG_DOWN … TAG_UP` (`Kernel/nodepath.cpp:2640-2683`).
* An attribute applied to a **group** or to a **layer** is inherited by all its
  descendants unless they redefine it (the classic inheritance model of Camelot's document
  tree: `Node::FindAppliedAttribute`).
* Therefore, in order to render, the importer must maintain an **attribute context per
  tree level**: on entering a `TAG_DOWN`, clone (or push) the context; on leaving at
  `TAG_UP`, restore it; and the attribute records at the current level modify the local
  copy.
* The `TAG_CURRENTATTRIBUTES` (4119) records and their children are **not** attributes
  applied to objects: they are the editor's *current* attributes (what would be applied to
  the next object the user draws). The loader switches into a "make current" insert mode
  for them (`Kernel/rechdoc.cpp:1942-1966`). They are **not** the default attributes an
  unattributed object inherits: those are the factory defaults registered at start-up
  (`docs/research/02-document-model.md` §4.4), which no file overrides. Taking them for
  defaults is a real bug with a visible symptom: `Designs/SimpleSphere.xar`'s current fill
  is black, and its last object, a filled-and-stroked frame with no fill attribute, then
  covers the whole design. They form an **atomic** node: if they are of no interest, the
  whole subtree must be discarded.
* **The factory default fill is "no colour"**: a flat fill whose colour is
  `COLOUR_NONE`, i.e. an unattributed filled path is not filled at all
  (`Kernel/fillval.cpp:692-703`). The default transparency is flat, level 0
  (`Kernel/fillval.cpp:4320-4329`).

```rust
#[derive(Clone, Default)]
pub struct AttrCtx {
    pub fill: Option<Fill>,
    pub line_colour: Option<Ref>,
    pub line_width: i32,          // millipoints
    pub start_cap: CapStyle,
    pub join: JoinStyle,
    pub mitre_limit: i32,
    pub winding: WindingRule,
    pub dash: Option<Dash>,
    pub start_arrow: Option<Arrow>,
    pub end_arrow: Option<Arrow>,
    pub fill_transp: Option<Transparency>,
    pub line_transp: Option<Transparency>,
    pub feather: Option<Feather>,
    pub text: TextAttrs,
}
// Traversal: at DOWN -> stack.push(ctx.clone());  at UP -> ctx = stack.pop();
```

### 8.2. Line attributes

| tag | payload | notes |
|---|---|---|
| 151 `TAG_LINECOLOUR` | `i32 colour_ref` | §9 |
| 193/194/195 `TAG_LINECOLOUR_{NONE,BLACK,WHITE}` | empty | shortcuts for references −1/−2/−3 (`Kernel/lineattr.cpp:928-950`) |
| 152 `TAG_LINEWIDTH` | `i32 width` (millipoints; 0 = "hairline" width) | `Kernel/lineattr.cpp:539-553` |
| 174 `TAG_STARTCAP` | `u8 cap` | the original's `LineCapType` as a byte: 0=butt, 1=round, 2=square (`Kernel/attr.h:126-131`; written and read raw, `Kernel/lineattr.cpp:1863`, `:4174`). The `CXF_CAP_STYLE_*` values 1..3 in `Kernel/cxfdefs.h:132-134` are never used (corrected by XARA-T-0329: the corpus holds 1 and 2 only, never the default 0) |
| 175 `TAG_ENDCAP` | `u8 cap` | same values. They are written as a pair (`Kernel/lineattr.cpp:1856-1880`) |
| 176 `TAG_JOINSTYLE` | `u8 join` | the original's `JointType` as a byte: 0=mitre, 1=round, 2=bevelled (`Kernel/attr.h:147-152`; raw, `Kernel/lineattr.cpp:2904`, `:4210`), not the unused `CXF_JOIN_STYLE_*` 1..3. The document default is **bevelled** (`Kernel/attr.cpp:1967-1980`) and is never written: the corpus holds 0 and 1 only (XARA-T-0329) |
| 177 `TAG_MITRELIMIT` | `i32` | `Kernel/lineattr.cpp:3191-3205` |
| 178 `TAG_WINDINGRULE` | `u8 rule` | the original's `WindingType` as a byte: 0=nonzero, 1=negative, 2=evenodd, 3=positive (`Kernel/attr.h:169-175`; written and read raw, `Kernel/lineattr.cpp:3506`, `:4276`), not the unused `CXF_WINDING_RULE_*` 1..4. The document default is **even-odd** (`Kernel/attr.cpp:1856-1858`) and is never written. Every renderer passes the rasteriser one flag, even-odd or not (`wxOil/grndrgn.cpp:2233-2234`, `:2769`, `:3798`, `:7114`; `wxOil/osrndrgn.cpp:1318-1321`), so negative and positive **draw as non-zero**. The corpus holds three records, all 1 (negative), on three quadrilaterals of `Designs/scope3 simple.xar` (records 7956/7966/7976), and no 2 (XARA-T-0331) |
| 173 `TAG_LINETRANSPARENCY` | `u8 transp`, `u8 type` | §8.5 |
| 179 `TAG_QUALITY` | `i32 quality` | render quality |

### 8.3. Colour fills

All gradients share a geometric prefix of 2 or 3 `DocCoord`s:

* `start_point` — origin of the gradient.
* `end_point` — end of the main axis.
* `end_point2` — end of the secondary axis (elliptical, square, 3/4-colour, bitmap,
  fractal, noise and three-point linear only).

and a **profile** suffix `f64 bias`, `f64 gain` (added in Xara X; this is why the sizes
"grew by +16" in `Kernel/cxfdefs.h:249-262`).

| tag | size | payload |
|---|---:|---|
| 150 `TAG_FLATFILL` | 4 | `i32 colour_ref` |
| 190/191/192 `TAG_FLATFILL_{NONE,BLACK,WHITE}` | 0 | shortcuts (`Kernel/fillattr.cpp:5941-5960`) |
| 153 `TAG_LINEARFILL` | 40 | `Coord start`, `Coord end`, `i32 col_start`, `i32 col_end`, `f64 bias`, `f64 gain` (`Kernel/fillattr.cpp:6805-6815`) |
| 4121 `TAG_LINEARFILL3POINT` | 48 | the same + `Coord end2` after `end` |
| 154 `TAG_CIRCULARFILL` | 40 | as 153 (`Kernel/fillattr.cpp:8066-8080`) |
| 155 `TAG_ELLIPTICALFILL` | 48 | `start`, `end`, `end2`, `col_start`, `col_end`, `bias`, `gain` |
| 156 `TAG_CONICALFILL` | 40 | as 153 (`Kernel/fillattr.cpp:9502-9512`) |
| 200 `TAG_SQUAREFILL` | 48 | as 155 (`Kernel/fillattr.cpp:10622-10632`) |
| 202 `TAG_THREECOLFILL` | 36 | `start`, `end`, `end2`, `col_start`, `col_end`, `col_end2` (**no profile**) |
| 204 `TAG_FOURCOLFILL` | 40 | `start`, `end`, `end2`, `col_start`, `col_end`, `col_end2`, `col_end3` |
| 157 `TAG_BITMAPFILL` | 44 | `start`, `end`, `end2`, `i32 bitmap_ref`, `f64 bias`, `f64 gain` |
| 158 `TAG_CONTONEBITMAPFILL` | 52 | `start`, `end`, `end2`, `i32 col_start`, `i32 col_end`, `i32 bitmap_ref`, `bias`, `gain` (`Kernel/fillattr.cpp:15290-15306`) |
| 159 `TAG_FRACTALFILL` | 69 | `start`, `end`, `end2`, `col_start`, `col_end`, `i32 seed`, `FIXED16 graininess`, `FIXED16 gravity`, `FIXED16 squash`, `i32 dpi`, `u8 tileable`, `bias`, `gain` (`Kernel/fillattr.cpp:16929-16946`) |
| 4010 `TAG_NOISEFILL` | 61 | `start`, `end`, `end2`, `col_start`, `col_end`, `FIXED16 graininess`, `i32 seed`, `i32 dpi`, `u8 tileable`, `bias`, `gain` (`Kernel/fillattr.cpp:17143-17157`) |

**Multi-stage gradients** (4075–4078, 4088, 4122) — `Kernel/fillattr.cpp:6827-6845`:
the same geometric prefix and the two end colours, followed by

```
u32 num_ramp_items
per item: f64 position (0..1), i32 colour_ref
```

A real example: an 88-byte `TAG_LINEARFILLMULTISTAGE` = 16 (2 coords) + 8 (2 refs)
+ 4 (the count) + 5 × 12 (a ramp with 5 stops).
⚠️ **Multi-stage fills carry no bias/gain**, unlike their two-colour equivalents.

**Fill mapping / repetition** (empty records that modify the preceding fill,
`Kernel/fillattr.cpp:18456-18485`):

| tag | internal `Repeat` | meaning |
|---|---|---|
| 164 `TAG_FILL_NONREPEATING` | 0/1 | do not repeat |
| 163 `TAG_FILL_REPEATING` | 2 | repeat |
| 165 `TAG_FILL_REPEATINGINVERTED` | 3 | repeat inverted (mirrored) |
| 206 `TAG_FILL_REPEATING_EXTRA` | 4 | "extra" repeat |

On reading, `TAG_FILL_NONREPEATING` becomes 1, never 0
(`Kernel/fillattr.cpp:22631-22660`; the transparency records 180/181/182/207 likewise,
`:22677-22705`). Value 4 exists because the build defines `NEW_FEATURES`
(`Makefile.am:5`). **The factory default is 2, "repeat"**, for both the colour and the
transparency mapping (`Kernel/fillval.cpp:7942-7945`, `:8358-8361`), so an object with no
mapping record has mapping 2.

**How the mapping renders** depends on the fill family, and the value is *not* taken at
face value (`wxOil/grndrgn.cpp`):

| Fill family | Mapping 0–3 | Mapping 4 | Reference |
|---|---|---|---|
| Graduated: linear, circular/elliptical, conical, square (diamond) | **ignored — the ramp clamps** to its end colours | tiles, with a long "high-quality repeat" ramp table | `:2585`, `:2683-2740` (colour); `:3967-3995`, `:4159-4161` (transparency) |
| Three- and four-colour | 1 → simple (clamped); 0, 2, 3 → tiled | tiled | `:2636-2640`, `:2665-2669` (colour); `:4125-4145` (transparency) |
| Bitmap and procedural | passed through as the tiling style | passed through | `:3538-3539` (colour); `:4302-4304` (transparency) |

The practical consequence: a gradient with the default mapping, or with an explicit
`TAG_FILL_REPEATING`, does **not** repeat. Only `TAG_FILL_REPEATING_EXTRA` makes one tile.

**Colour interpolation effect** (empty records): 160 `FILLEFFECT_FADE` (linear RGB),
161 `FILLEFFECT_RAINBOW` (HSV, short way round), 162 `FILLEFFECT_ALTRAINBOW` (HSV, long
way round).

### 8.4. Transparencies

The same geometry as their colour counterparts, but with 1-byte transparency levels
(0 = opaque … 255 = fully transparent) instead of colour references:

| tag | size | payload |
|---|---:|---|
| 166 `TAG_FLATTRANSPARENTFILL` | 2 | `u8 transp`, `u8 type` |
| 167 `TAG_LINEARTRANSPARENTFILL` | 35 | `start`, `end`, `u8 t0`, `u8 t1`, `u8 type`, `f64 bias`, `f64 gain` |
| 4123 `TAG_LINEARTRANSPARENTFILL3POINT` | 43 | the same + `end2` |
| 168 `TAG_CIRCULARTRANSPARENTFILL` | 35 | as 167 |
| 169 `TAG_ELLIPTICALTRANSPARENTFILL` | 43 | `start`, `end`, `end2`, `t0`, `t1`, `type`, `bias`, `gain` |
| 170 `TAG_CONICALTRANSPARENTFILL` | 35 | as 167 |
| 201 `TAG_SQUARETRANSPARENTFILL` | 43 | as 169 |
| 203 `TAG_THREECOLTRANSPARENTFILL` | 28 | `start`, `end`, `end2`, `t0`, `t1`, `t2`, `type` |
| 205 `TAG_FOURCOLTRANSPARENTFILL` | 29 | `…`, `t0`, `t1`, `t2`, `t3`, `type` |
| 171 `TAG_BITMAPTRANSPARENTFILL` | 47 | `start`, `end`, `end2`, `t0`, `t1`, `type`, `i32 bitmap_ref`, `bias`, `gain` |
| 172 `TAG_FRACTALTRANSPARENTFILL` | 64 | like the fractal one but with `t0,t1,type` instead of colours |
| 4011 `TAG_NOISETRANSPARENTFILL` | 56 | like the noise one with `t0,t1,type` |
| 180/181/182/207 | 0 | mapping repeat / do not repeat / repeat inverted / extra |

**Transparency types** (`Kernel/cxfdefs.h:121-131`) — the `type` byte:

| value | mode |
|---:|---|
| 0 | none (opaque) |
| 1 | mix (normal / alpha) |
| 2 | stained glass (multiply) |
| 3 | bleach (screen) |
| 13 | contrast |
| 16 | saturation |
| 19 | darken |
| 22 | lighten |
| 25 | brightness |
| 28 | luminosity |

The gaps (4–12, 14–15, …) correspond to internal variants of the render engine that are
not documented in the code; **treat unknown values as `mix`**.

### 8.5. Dashes

* **`TAG_DASHSTYLE` (183), 4 bytes**: `i32 dash_ref`. If negative, it is one of the
  predefined patterns (−1..−20, −21 solid, −22 guide layer,
  `Kernel/cxfdash.h:104-126`, `Kernel/lineattr.cpp:3830-3862`).
* **`TAG_DEFINEDASH` (184) / `TAG_DEFINEDASH_SCALED` (188)**, variable
  (`Kernel/lineattr.cpp:3868-3902`) — **this is not a referenceable definition**: it
  replaces `TAG_DASHSTYLE` and defines the pattern *in situ*:

| Type | Field |
|---|---|
| `i32` | `dash_start` (initial offset, millipoints) |
| `i32` | reference `line_width` |
| `i32` | `num_elements` |
| `i32[num_elements]` | alternating dash/gap lengths (millipoints) |

The `_SCALED` variant indicates that the pattern scales with the line width.

### 8.6. Arrowheads

**`TAG_ARROWHEAD` (185)** and **`TAG_ARROWTAIL` (186)**, 12 bytes each
(`Kernel/lineattr.cpp:2203-2245`):

| Off | Type | Field |
|---|---|---|
| 0 | `i32` | `arrow_ref` — negative = predefined arrowhead (§6.3) |
| 4 | `FIXED16` | width scale |
| 8 | `FIXED16` | height scale |

### 8.7. Miscellaneous attributes

| tag | payload |
|---|---|
| 179 `TAG_QUALITY` | `i32` |
| 189 `TAG_USERVALUE` | `UNICODE-Z key`, `UNICODE-Z value`. If the key is the resource string `IDS_USERATTRKEY_WEBADDRESS`, it is an old-style hyperlink (`Kernel/rechattr.cpp:240-290`) |
| 4086 `TAG_FEATHER` | `i32 size`, `f64 bias`, `f64 gain` |
| 3500–3505 | overprint attributes, no payload |

---

## 9. Colours

### 9.1. Colour models

`Kernel/colmodel.h:199-215` (the `model` byte of `TAG_DEFINECOMPLEXCOLOUR`):

| value | model | components (4 × FIXED24) |
|---:|---|---|
| 0 | `COLOURMODEL_INDEXED` | (internal use, not saved) |
| 1 | `COLOURMODEL_CIET` | X, Y, Z, transparency |
| 2 | `COLOURMODEL_RGBT` | R, G, B, transparency |
| 3 | `COLOURMODEL_CMYK` | C, M, Y, K |
| 4 | `COLOURMODEL_HSVT` | H, S, V, transparency |
| 5 | `COLOURMODEL_GREYT` | intensity, 0, 0, 0 |
| 6 | `COLOURMODEL_WEBRGBT` | as RGBT, restricted to the web palette |

All components are **FIXED24 normalised to 0.0–1.0** (`raw / 2^24`), except the hue (H) of
HSV, which is also normalised 0..1 (equivalent to 0–360°).
The special value `FIXED24(-8.0)` = `0xF8000000` marks a component **inherited** from the
parent colour in linked colours (`Kernel/colcomp.cpp:2044-2058`).

### 9.2. `TAG_DEFINECOMPLEXCOLOUR` (51)

Writing `Kernel/colcomp.cpp:1840-2010`, reading `Kernel/colcomp.cpp` +
description in `Kernel/rechcol.cpp:259-310`.

| Off | Type | Field |
|---:|---|---|
| 0 | `u8` | `r` — 8-bit RGB approximation (for fast rendering and for simple readers) |
| 1 | `u8` | `g` |
| 2 | `u8` | `b` |
| 3 | `u8` | `colour_model` (table in §9.1) |
| 4 | `u8` | `colour_type` (table in §9.3) |
| 5 | `u32` | `entry_index` — position in the document's colour list (0 if it is not on the colour line) |
| 9 | `i32` | `parent_ref` — **record number** of the parent colour (0 = none) |
| 13 | `FIXED24` | component 1 |
| 17 | `FIXED24` | component 2 |
| 21 | `FIXED24` | component 3 |
| 25 | `FIXED24` | component 4 |
| 29 | `UNICODE-Z` | colour name (may be the empty string → 2 bytes) |

Size = 29 + 2·(len+1). Observed: from 31 (no name) up to 65.

### 9.3. Colour types and derived colours

`Kernel/colcomp.h:155-159`:

| value | type | semantics of the components |
|---:|---|---|
| 0 | `NORMAL` | independent colour |
| 1 | `SPOT` | spot colour (its own separation) |
| 2 | `TINT` | **tint** of the colour `parent_ref`: comp1 = tint factor (0..1) |
| 3 | `LINKED` | colour linked to the parent; components with the value `0xF8000000` are inherited from the parent, the rest override it |
| 4 | `SHADE` | **shade** of the parent: comp1 = shade value X, comp2 = value Y (`Kernel/colcomp.cpp:1968-1983`) |

Real examples taken from `testfiles/OneLine.xar`:

```
record #31: rgb=(0,0,0)     model=3 (CMYK) type=0 entry=24 parent=0  comps=[0,0,0,1]      name="Black"
record #45: rgb=(255,0,0)   model=4 (HSVT) type=0 entry=0  parent=0  comps=[0,1,1,0]      name="Red"
record #69: rgb=(25,25,25)  model=3 (CMYK) type=2 entry=25 parent=31 comps=[0.9,0,0,0]    name="90% Black"
                                                       ^a 90 % tint of record 31 (Black)
```

### 9.4. Recommended implementation strategy

For an importer that only needs to paint:

1. Keep a `HashMap<u32 /*record*/, ColourDef>`.
2. When resolving a reference:
   * `< 0` → predefined colour from the table in §6.3;
   * `0` → no colour;
   * `> 0` → look it up in the map; if it is not there (corrupt file) → black.
3. To obtain RGB, **the first 3 bytes of the record are enough** (`r`,`g`,`b`), which Xara
   has already computed. This is exactly what Xara's simplified importer does for
   previewing. Use the full model only if CMYK fidelity, separations or editing are
   needed.
4. Resolving tints/shades/links requires walking the `parent_ref` chain. Since the parent
   is **always written before** the child (`Kernel/colcomp.cpp:1900-1910` guarantees it
   with an `ERROR2IF`), a single-pass incremental map is enough.

### 9.5. Colour profiles

**The format stores no ICC profiles.** There is no profile tag at all in `cxftags.h`.
The closest thing is:

* Per-plate colour separation information (`TAG_COLOURPLATE`, 3508), with the plate type,
  the associated spot colour, and the screen angle and frequency.
* The per-colour model (CMYK vs RGB), which determines the nominal space.

The CMYK→RGB conversion in the original code is the trivial one
(`R = 1−min(1, C+K)`, and so on, via `ColourContext`), with no colour management.

---

## 10. Implementation priorities

### 10.1. Methodology

**59 `.xar` files** from the original repository were analysed (all of those in
`testfiles/`, `Designs/`, `Templates/` and `TextDesigns/`), with a total of
**1 390 282 records** and **157 distinct tags** (out of the 300 defined). The script is in
§12.1 and the full results in §12.2.

### 10.2. Level 0 — essential (without this nothing can be read)

```
magic + TAG_FILEHEADER(2) + TAG_ENDOFFILE(3)
TAG_STARTCOMPRESSION(30) + TAG_ENDCOMPRESSION(31)      <- raw deflate
TAG_UP(0) + TAG_DOWN(1)                                <- 28.9 % of all records
```

### 10.3. Level 1 — 95 % of the real records

The following **16 tags** account for **94.96 %** of all the records in the corpus, and
with the 17th (`TAG_GROUP`) the figure reaches 96.33 %:

| # | tag | name | % | cumulative | files |
|---:|---:|---|---:|---:|---:|
| 1 | 1 | `TAG_DOWN` | 14.47 | 14.47 | 59/59 |
| 2 | 0 | `TAG_UP` | 14.47 | 28.93 | 59/59 |
| 3 | 111 | `TAG_PATH_FLAGS` | 10.96 | 39.89 | 35 |
| 4 | 152 | `TAG_LINEWIDTH` | 9.94 | 49.83 | 59/59 |
| 5 | 116 | `TAG_PATH_RELATIVE_FILLED_STROKED` | 9.73 | 59.55 | 28 |
| 6 | 193 | `TAG_LINECOLOUR_NONE` | 5.32 | 64.88 | 55 |
| 7 | 150 | `TAG_FLATFILL` | 4.97 | 69.84 | 57 |
| 8 | 176 | `TAG_JOINSTYLE` | 4.74 | 74.59 | 25 |
| 9 | 151 | `TAG_LINECOLOUR` | 4.55 | 79.14 | 49 |
| 10 | 153 | `TAG_LINEARFILL` | 4.02 | 83.16 | 18 |
| 11 | 1901 | `TAG_REGULAR_SHAPE_PHASE_2` | 2.53 | 85.69 | 22 |
| 12 | 155 | `TAG_ELLIPTICALFILL` | 2.10 | 87.79 | 13 |
| 13 | 174 | `TAG_STARTCAP` | 1.89 | 89.68 | 7 |
| 14 | 175 | `TAG_ENDCAP` | 1.89 | 91.56 | 7 |
| 15 | 173 | `TAG_LINETRANSPARENCY` | 1.74 | 93.31 | 21 |
| 16 | 166 | `TAG_FLATTRANSPARENTFILL` | 1.65 | 94.96 | 55 |
| 17 | 104 | `TAG_GROUP` | 1.37 | 96.33 | 23 |
| 18 | 115 | `TAG_PATH_RELATIVE_STROKED` | 1.21 | 97.55 | 25 |
| 19 | 167 | `TAG_LINEARTRANSPARENTFILL` | 0.85 | 98.39 | 15 |
| 20 | 51 | `TAG_DEFINECOMPLEXCOLOUR` | 0.41 | 98.81 | **59/59** |
| 21 | 169 | `TAG_ELLIPTICALTRANSPARENTFILL` | 0.38 | 99.19 | 11 |

**But "95 % of the records" is not the same as "95 % of the files read correctly".** There
are infrequent tags that are present in **every** file and are structural. The **real
minimum set** for opening any `.xar` in the corpus and rendering it recognisably is the
union of the above with those that appear in 59/59 files:

```
Structure:    0,1,2,3,10,30,31,40,41,42,43,45,46,47,48,80,82,87,91,92,93,4070,4087,4114,4116,4031
Colour:       51  (+ the predefined negative references)
Geometry:     111,115,116,1901,104
Attributes:   150,151,152,153,155,166,167,169,173,174,175,176,193
```

That is, **~45 tags** cover 99.2 % of the records and 100 % of the 59 files.

### 10.4. Level 2 — needed for "real" documents

Add, in order of return on effort:

1. **Bitmaps**: 68/67/71/65/66 (definition), 198 (object), 157/158 (fills), 4115.
   Present in 22–24 files of the corpus. Dumping the embedded PNG/JPEG is enough.
2. **Text**: 2000, 2100/2101, 2200, 2201, 2202, 2203, 2206, 2150, 2151, 2906, 2907,
   2900, 2919, 2920, 2902-2905, 2908-2917, 2918. 22 files of the corpus.
3. **Remaining gradients**: 154, 156, 200, 202, 204, 159, 4010 and their transparencies;
   mappings 160-165, 180-182, 206, 207.
4. **Composite groups**: 105/106/4072/4073 (blends), 107-110/4012 (moulds),
   4050/4051 (shadows), 4052/4057 (bevels), 4066/4067 (contours), 4086 (feather),
   4128 (compound render).
5. **Dashes and arrowheads**: 183, 184, 188, 185, 186.

### 10.5. Level 3 — rare or legacy (implement only if needed)

* **Never observed in the corpus but defined and with code behind it**: absolute paths
  (100-103), relative paths 113/114, `TAG_DEFINERGBCOLOUR` (50), legacy regular shapes
  (1000-1217, 1900), `TAG_PAGE` (44), `TAG_GUIDELINE` (112),
  `TAG_PATHREF_*` (118, 4013), `TAG_DEFINEDASH` (184), `TAG_TEXT_TAB`/`RULER`/indents
  (4200-4204), `TAG_DOCUMENTCOMMENT` (90), `TAG_VIEWQUALITY` (81),
  `TAG_CLIPVIEW*` (4084/4085), `TAG_LIVE_EFFECT` (4125-4127), `TAG_OBJECTBOUNDS` (4129),
  XPE (4117/4118).
* **Declared without an implementation** (§4.11): 21 tags. Treat them as unknown.
* **Printing/imagesetting** (3500-3510): irrelevant to rendering; skip.
* **Brushes and strokes** (4000-4003, 4079-4083, 4102-4113): very complex and present in
  a single file of the corpus; the visual result can be approximated by ignoring them
  (the base path is still in the file).

### 10.6. Suggested order of work

```
1. Physical reader: magic, records, deflate + CRC        -> validates all 59 files
2. DOWN/UP tree + tag dump                               -> the `xar-dump` tool
3. Colours (51) + negative refs
4. Relative paths (115/116) + PATH_FLAGS + verbs
5. Line attributes and flat fill                         -> something useful already renders
6. Linear/elliptical gradients + flat transparencies
7. Regular shapes 1901 (ellipses and rectangles)
8. Groups and layers (attribute inheritance)
9. Bitmaps
10. Text
11. Composite effects
```

---

## 11. Known risks and ambiguities

1. **Record size vs. version.** Several records have grown over time
   (`TAG_LINEARFILL` 24→40, `TAG_TEXT_STORY_SIMPLE` 8→12, `TAG_SHADOW` 16→24,
   `TAG_BLENDERADDITIONAL` 16→17, `TAG_FEATHER` 4→20…). **Never assume the declared
   size**: read fields while bytes remain and apply default values
   (bias=0, gain=0, autokern=0…). The original code uses tolerant reads
   (`ReadINT32noError`, `Kernel/cxfrec.cpp:1290`).

2. **Discrepancies between `cxfdefs.h` and the actual writers.**
   * `TAG_VARIABLEWIDTHFUNC_SIZE = 4` but the writer emits 8 bytes
     (`Kernel/strkattr.cpp:1343-1347`).
   * `TAG_BEVEL` has no size constant; the writer uses a hard-coded 24
     (`Kernel/nbevcont.cpp:507`).
   * `TAG_DOCUMENTNUDGE` uses the constant `TAG_DOCUMENTNUDGESIZE` (with no underscore).
   → **The source of truth is the writer**, not the constant.

3. **Double-page bit in `TAG_SPREADINFORMATION`**: the import handler uses bit 0 and the
   debug code bit 2 (§4.3). Following the handler is recommended.

4. **Relative paths with no count**: if `size % 9 != 0` the record is corrupt or belongs
   to an unknown variant. Abort the reading of that path, do not guess.

5. **Sign of the delta in relative paths**: it is `P[i] = P[i-1] − D`. A sign error
   produces mirrored geometry and is hard to spot on symmetric shapes.

6. **`RELPATHINTERLEAVE`**: the code contains two alternative formats for relative paths
   (interleaved and non-interleaved, `Kernel/cxfrec.cpp:1763,1846`). In Xara LX the macro
   is **defined**, so the real format is the interleaved one. Files written by a
   hypothetical version with the macro switched off (verbs grouped at the start + normal
   coordinates) **would not be readable** with the interleaved reader, and **there is no
   way to tell them apart from the tag**. No such file has been found in the corpus; the
   risk is theoretical.

7. **Tags from later Xara versions (Xtreme Pro, Designer Pro).** `TAG_SPREAD_PHASE2`
   (4131), `TAG_CURRENTATTRIBUTES_PHASE2` (4132) and the whole range >4138 may appear in
   files created by more recent commercial versions. The `TAG_ATOMICTAGS` /
   `TAG_ESSENTIALTAGS` mechanism is designed for exactly that: **implement it from the
   start** or trees will be corrupted when reading modern files.

8. **FIXED16 precision in matrices.** Scales and rotations have 16 fractional bits. When
   several transformations are composed the error accumulates. It is best to work in
   `f64` internally and not requantise.

9. **Inherited colour components.** The value `0xF8000000` (FIXED24 −8.0) is not a valid
   colour: it means "inherit from the parent". Interpreting it literally yields absurd
   colours.

10. **Streamed records and compression.** If the writer is implemented, the closing and
    reopening of the compressed block around each bitmap must be respected; otherwise the
    original Xara will not be able to read the file. For the **reader** it does not
    matter.

11. **The CRC covers only the compressed block.** There is no checksum for the whole file
    or for the uncompressed records (header, preview, bitmaps). A `.xar` truncated inside
    a bitmap is not detected until the next record fails.

12. **`TAG_ENDOFFILE` need not be the last byte.** The reader must stop there and not
    assume that `pos == len`. (In the corpus it always coincides: 0 trailing bytes across
    all 59 files.)

13. **Atomic nodes with derived children.** If `TAG_SHADOWCONTROLLER` is ignored but its
    children are inserted (`TAG_SHADOW` + the duplicated object), the shadow will be
    painted as if it were an independent object. The atomic semantics must be respected.

14. **Inconsistent angle units.** `ANGLE` is FIXED16 in radians, but
    `TAG_SHADOWCONTROLLER` stores its angle as an `i32` with a bespoke encoding
    (`FloorAngleToINT32`, `Kernel/nodecont.cpp:1633`) and `TAG_BEVEL` uses whole degrees.
    Check case by case.

15. **Unit of `TAG_TEXT_TRACKING`.** The C++ type is `MILLIPOINT`
    (`Kernel/txtattr.h:413`) but the value is combined with the font size when rendering;
    it is worth calibrating against a reference render before fixing the conversion.
    *Settled (phase 9):* thousandths of the em width, `MulDiv(tracking, FontEmWidth, 1000)`
    (`Kernel/nodetext.cpp:1781-1792`); `TAG_TEXT_KERN` values likewise (`:1763-1767`).

16. **Strings**: `TAG_TEXT_STRING` carries no terminator; the rest do. Mixing the two
    conventions causes desynchronisation within the record.

---

## 12. Appendices

### 12.1. Validation script (minimal parser in Python)

Location: `scratchpad/xarparse.py` + `scratchpad/dump.py`
(the session's working directory). Core of the parser:

```python
import struct, zlib

MAGIC = b'XARA\xa3\xa3\x0d\x0a'

def records(path):
    data = open(path, 'rb').read()
    assert data[:8] == MAGIC
    pos, dec, buf, bpos, crc, total = 8, None, b'', 0, 0, 0
    def read(n):
        nonlocal pos, buf, bpos, crc, total
        if dec is None:
            d = data[pos:pos+n]; pos += len(d); return d
        while len(buf) - bpos < n and not dec.eof:
            chunk = data[pos:pos+4096]; pos += len(chunk)
            buf += dec.decompress(dec.unconsumed_tail + chunk)
            if not chunk: break
        d = buf[bpos:bpos+n]; bpos += len(d)
        crc = zlib.crc32(d, crc); total += len(d)
        return d
    while True:
        h = read(8)
        if len(h) < 8: return
        tag, size = struct.unpack('<II', h)
        if tag == 30:                       # STARTCOMPRESSION
            ver = struct.unpack('<I', read(size))[0]
            dec = zlib.decompressobj(-15)   # RAW deflate
            buf, bpos, crc, total = b'', 0, 0, 0
            yield tag, size, b''; continue
        if tag == 31:                       # ENDCOMPRESSION
            crc_calc, len_calc = crc & 0xffffffff, total
            unused = dec.unused_data; dec = None
            pos -= (len(unused) - 8)        # the trailer is UNCOMPRESSED
            crc_f, len_f = struct.unpack('<II', data[pos-8:pos])
            assert (crc_f, len_f) == (crc_calc, len_calc)
            yield tag, size, b''; continue
        payload = read(size) if size else b''
        yield tag, size, payload
        if tag == 3: return                 # ENDOFFILE
```

Result over the 59 files: **0 errors**, correct CRC and length in all 103 compressed
blocks, 0 trailing bytes left over in every file.

### 12.2. Results of the corpus analysis

**Files analysed** (59): `testfiles/*.xar` (18), `Designs/*.xar` (19),
`Templates/*.xar` (8), `TextDesigns/*.xar` (14).

* All of them are of type **`CXN`** (native).
* All of them use **compression** with `compression_version = 99` (zlib 0.99, type 0 =
  deflate).
* All of them have a GIF preview except one.
* Producers observed (the `producer` / `producer_version` / `producer_build` fields):

| times | producer | version | build |
|---:|---|---|---|
| 17 | `Xara Xtreme` | 3.0 | `0.4366 (Xara)` |
| 14 | `Xara Xtreme` | 3.0 | `0.4480 (Xara)` |
| 8 | `Xara Xtreme` | 3.0 | `0.4224 (Gerry)` |
| 6 | `Xara X` | 3.0 | `0.2704 (MarkG)` |
| 6 | `Xara Xtreme` | 3.0 | `0.4308 (SimonM)` |
| 3 | `Xara Xtreme` | 3.0 | `0.3993 (Gavin)` |
| 3 | `Xara Xtreme` | 3.0 | `0.4372 (Xara)` |
| 1 | `Xara Xtreme` | 3.0 | `0.4293 (SimonM)` |
| 1 | `X` | *(empty)* | *(empty)* |

  ⚠️ The last case (`Templates/animation.xar`, a header of only 36 bytes) shows that the
  three strings may be empty or truncated: **the reader must not require them**.
* Maximum tree depth: 4 (templates) … 13 (`ProbeX16.xar`).
* Total: 1 390 282 records, 157 distinct tags.

**Observed vs. declared payload sizes**: they match for 100 % of the fixed-size tags.
A sample:

| tag | declared | observed |
|---|---|---|
| 45 `SPREADINFORMATION` | 17 | 17 |
| 51 `DEFINECOMPLEXCOLOUR` | 29 + name | 31, 37, 41, 43, 49, 51, 53, 55, 57, 63, 65 |
| 105 `BLEND` | 3 | 3 |
| 106 `BLENDER` | 8 | 8 |
| 153 `LINEARFILL` | 40 | 40 |
| 155 `ELLIPTICALFILL` | 48 | 48 |
| 159 `FRACTALFILL` | 69 | 69 |
| 166 `FLATTRANSPARENTFILL` | 2 | 2 |
| 167 `LINEARTRANSPARENTFILL` | 35 | 35 |
| 1901 `REGULAR_SHAPE_PHASE_2` | var | 119 (35 178×), 155 (28×) |
| 2000 `FONT_DEF_TRUETYPE` | var | 34, 36, 54, 74, 78 |
| 4050 `SHADOWCONTROLLER` | 29 | 29 |
| 4051 `SHADOW` | 24 | 24 |
| 4052 `BEVEL` | (24, not declared) | 24 |
| 4073 `BLENDERADDITIONAL` | 17 | 17 |
| 4086 `FEATHER` | 20 | 20 |
| 4115 `BITMAP_PROPERTIES` | 12 | 12 |

**A complete example — `testfiles/OneLine.xar`** (2 261 bytes, 88 records, depth 5):

```
#1   TAG_FILEHEADER              41   CXN / 3996 / "Xara X" "3.0" "0.2704 (MarkG)"
#2   TAG_PREVIEWBITMAP_GIF     1196   GIF87a...
#3   TAG_DOCUMENT                 0
#4   TAG_DOWN                     0
#5   TAG_DOCUMENTNUDGE            4
#6   TAG_DOCUMENTBITMAPSMOOTHING  5
#7   TAG_STARTCOMPRESSION         4   version=99  -> from here on, deflate
#8   TAG_VIEWPORT                16
#9..17 TAG_ATOMICTAGS             4   (9 records, one tag each)
#18  TAG_CHAPTER                  0
#19  TAG_DOWN / #20 TAG_SPREAD / #21 TAG_DOWN
#22  TAG_SPREADINFORMATION       17   600000 x 450000 mp, margin 576000, bleed 0, flags 2
#23  TAG_SPREADSCALING_INACTIVE  24
#24  TAG_SPREAD_ANIMPROPS        28
#25  TAG_LAYER 0 / #26 TAG_DOWN
#27  TAG_LAYERDETAILS            17   flags=0x0D, "Layer 1"
#28  TAG_PATH_RELATIVE_STROKED   18   MoveTo(112101,178899) LineTo(283101,321399)
#29  TAG_DOWN
#30  TAG_PATH_FLAGS               2   0x05, 0x05  (smooth|endpoint)
#31  TAG_DEFINECOMPLEXCOLOUR     41   CMYK "Black"
#32  TAG_LINECOLOUR               4   -> record 31
#33  TAG_LINEWIDTH                4   500 mp
#34  TAG_FLATFILL                 4   -> record 31
#35  TAG_UP / #36 TAG_UP
#37  TAG_GRIDRULERSETTINGS       17
#38  TAG_GRIDRULERORIGIN          8
#39  TAG_UP
#40  TAG_SETSENTINEL / TAG_BARPROPERTY / units / dates / flags / printing / view
#87  TAG_ENDCOMPRESSION           8   CRC32 + uncompressed size (both verified OK)
#88  TAG_ENDOFFILE                0
```

### 12.3. Index of relevant C++ source files

| File | Content |
|---|---|
| `Kernel/cxftags.h` | **Complete list of tags** (300) |
| `Kernel/cxfdefs.h` | Magic, record sizes, cap/join/winding/transparency enums, layer/path/blend flags |
| `Kernel/cxfile.h/.cpp` | `CXaraFile` class: opening, magic, reading/writing records, compression, dispatch to handlers |
| `Kernel/cxfrec.h/.cpp` | `CXaraFileRecord` class: **serialisation of every primitive type**, coords, paths, matrices, strings |
| `Kernel/cxfrech.h/.cpp` | Base class of the *record handlers* + tag → readable name table |
| `Kernel/cxfmap.cpp` | Tag → handler map |
| `Kernel/cxftree.h/.cpp` | Debug dialog for the record tree (classification of tags by category) |
| `Kernel/camfiltr.h/.cpp` | `BaseCamelotFilter`: header, insertion tree, atomic/essential tags, coordinate origin |
| `Kernel/native.cpp`, `Kernel/webfiltr.cpp` | `CXN` / `CXW` / `CXM` filters |
| `Kernel/zstream.cpp`, `Kernel/ccfile.cpp` | zlib layer: raw deflate, CRC, trailer |
| `Kernel/cxfcols.h`, `cxfarrow.h`, `cxfdash.h`, `cxfunits.h` | Tables of predefined negative references |
| `Kernel/colcomp.cpp`, `rechcol.cpp`, `colmodel.h` | Colours |
| `Kernel/nodepath.cpp`, `GDraw/gconsts.h` | Paths and verbs |
| `Kernel/cxfellp/cxfrect/cxfpoly/cxfrgshp.cpp`, `rech*.cpp` | Regular shapes |
| `Kernel/fillattr.cpp` (23 k lines) | **All** the fills and transparencies |
| `Kernel/lineattr.cpp` | Line attributes, dashes, arrowheads |
| `Kernel/rechtext.cpp`, `cxftext.cpp`, `nodetxts.cpp`, `fontcomp.cpp` | Text and fonts |
| `Kernel/bmpcomp.cpp`, `rechbmp.cpp`, `cxfnbmp.cpp` | Bitmaps |
| `Kernel/rechdoc.cpp`, `spread.cpp`, `layer.cpp`, `viewcomp.cpp`, `unitcomp.cpp`, `infocomp.cpp` | Document, spreads, layers, views, units |
| `Kernel/nodeblnd.cpp`, `nodebldr.cpp`, `nodemold.cpp`, `nodeshad.cpp`, `nodecont.cpp`, `nbevcont.cpp`, `ncntrcnt.cpp`, `nodeclip.cpp`, `fthrattr.cpp`, `nodeliveeffect.cpp` | Effects and composite nodes |
| `Kernel/princomp.cpp`, `prnmkcom.cpp`, `isetattr.cpp` | Printing and imagesetting |
| `Kernel/cxftfile.h/.cpp` | **Text** format for Flare templates (out of scope) |

### 12.4. Skeleton of a Rust importer

```rust
//! Reader for .xar (CXF) files. Everything is little-endian.
//! Suggested dependencies: flate2 (raw inflate), thiserror.

pub const TAG_UP: u32 = 0;
pub const TAG_DOWN: u32 = 1;
pub const TAG_FILEHEADER: u32 = 2;
pub const TAG_ENDOFFILE: u32 = 3;
pub const TAG_ATOMICTAGS: u32 = 10;
pub const TAG_ESSENTIALTAGS: u32 = 11;
pub const TAG_STARTCOMPRESSION: u32 = 30;
pub const TAG_ENDCOMPRESSION: u32 = 31;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind { Native, Web, MinimalWeb }   // CXN / CXW / CXM

#[derive(Debug, Clone)]
pub struct FileHeader {
    pub kind: FileKind,
    pub uncompressed_size: u32,
    pub link_id: u32,
    pub precompression: u32,     // must be 0
    pub producer: String,
    pub producer_version: String,
    pub producer_build: String,
}

pub fn parse_file_header(data: &[u8]) -> Result<FileHeader, Err> {
    let mut c = Cur::new(data);
    let ty = c.take(3)?;
    let kind = match ty {
        b"CXN" => FileKind::Native,
        b"CXW" => FileKind::Web,
        b"CXM" => FileKind::MinimalWeb,
        _ => return Err(Err::BadFileType),
    };
    let uncompressed_size = c.u32()?;
    let link_id = c.u32()?;
    let precompression = c.u32()?;
    // The strings may be missing in old files: tolerate EOF.
    let producer = c.ascii_z().unwrap_or_default();
    let producer_version = c.ascii_z().unwrap_or_default();
    let producer_build = c.ascii_z().unwrap_or_default();
    Ok(FileHeader { kind, uncompressed_size, link_id, precompression,
                    producer, producer_version, producer_build })
}

/// Global state of the importer during a single pass.
pub struct Importer {
    pub header: Option<FileHeader>,
    pub origin: Coord,                          // CoordOrigin (set at SPREADINFORMATION)
    pub colours: HashMap<u32, ColourDef>,       // record -> colour
    pub bitmaps: HashMap<u32, BitmapDef>,
    pub fonts:   HashMap<u32, FontDef>,
    pub paths:   HashMap<u32, Vec<PathPoint>>,  // for TAG_PATHREF_*
    pub atomic:  HashSet<u32>,
    pub essential: HashSet<u32>,
    pub attr_stack: Vec<AttrCtx>,
    pub ctx: AttrCtx,
    skip_subtree_depth: Option<usize>,          // discarding of an atomic subtree
    depth: usize,
}

impl Importer {
    pub fn handle(&mut self, rec: &Record) -> Result<(), Err> {
        // 1) Handling the discarding of unknown atomic subtrees
        if let Some(d) = self.skip_subtree_depth {
            match rec.tag {
                TAG_DOWN => { self.depth += 1; return Ok(()); }
                TAG_UP => {
                    self.depth -= 1;
                    if self.depth <= d { self.skip_subtree_depth = None; }
                    return Ok(());
                }
                _ => return Ok(()),
            }
        }

        let mut c = Cur::new(&rec.data);
        match rec.tag {
            TAG_FILEHEADER => { self.header = Some(parse_file_header(&rec.data)?); }
            TAG_DOWN => { self.depth += 1; self.attr_stack.push(self.ctx.clone()); }
            TAG_UP   => { self.depth -= 1; if let Some(p) = self.attr_stack.pop() { self.ctx = p; } }
            TAG_ATOMICTAGS    => while c.remaining() >= 4 { self.atomic.insert(c.u32()?); },
            TAG_ESSENTIALTAGS => while c.remaining() >= 4 { self.essential.insert(c.u32()?); },

            45 => { /* SPREADINFORMATION: sets the page size and CoordOrigin */ }
            51 => { self.colours.insert(rec.number, parse_complex_colour(&mut c)?); }
            111 => { /* PATH_FLAGS: apply to the last path */ }
            113..=116 => {
                let pts = read_path_relative(&mut c, self.origin)?;
                self.paths.insert(rec.number, pts.clone());
                self.emit_path(pts, rec.tag);
            }
            100..=103 => {
                let pts = read_path_absolute(&mut c, self.origin)?;
                self.paths.insert(rec.number, pts.clone());
                self.emit_path(pts, rec.tag);
            }
            150 => { self.ctx.fill = Some(Fill::Flat(Ref::parse(c.i32()?))); }
            190 => { self.ctx.fill = Some(Fill::Flat(Ref::Builtin(-1))); }   // none
            191 => { self.ctx.fill = Some(Fill::Flat(Ref::Builtin(-2))); }   // black
            192 => { self.ctx.fill = Some(Fill::Flat(Ref::Builtin(-3))); }   // white
            151 => { self.ctx.line_colour = Some(Ref::parse(c.i32()?)); }
            193 => { self.ctx.line_colour = Some(Ref::Builtin(-1)); }
            152 => { self.ctx.line_width = c.i32()?; }
            174 => { self.ctx.start_cap = CapStyle::from(c.u8()?); }
            176 => { self.ctx.join = JoinStyle::from(c.u8()?); }
            166 => { let t = c.u8()?; let k = c.u8()?; self.ctx.fill_transp = Some(Transparency::flat(t, k)); }
            // ... the remaining tags ...
            TAG_ENDOFFILE => { /* end */ }
            unknown => {
                if self.essential.contains(&unknown) { return Err(Err::UnsupportedEssential(unknown)); }
                if self.atomic.contains(&unknown) { self.skip_subtree_depth = Some(self.depth); }
                // in any other case: the record is simply ignored
            }
        }
        Ok(())
    }
}
```

**Design notes:**

* `#[repr(C, packed)]` is **not** applicable to most payloads (unaligned fields and
  variable-length strings). It only makes sense for homogeneous blocks such as the four
  `FIXED24`s of a colour or the four `DocCoord`s of `TAG_NODE_BITMAP`, and even then the
  endianness has to be converted explicitly (`u32::from_le_bytes`), so the field-by-field
  reader shown in §2.5 is preferable.
* The importer must be **tolerant by default**: any record that is not understood is
  skipped using its `size`; only essential tags justify aborting.
* It is worth exposing a low-level API (`Iterator<Item = Record>`) independent of the
  document model, so that dump tools and round-trip tests can be written.

---

*End of document.*
