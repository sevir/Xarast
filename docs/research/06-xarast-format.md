# `.xarast` native format specification — version 1.0

> **Clean-room notice.** This document specifies the native `.xarast` format
> and describes, only where interoperability requires it, the *behaviour* and
> the *data formats* of Xara Xtreme (GPL-2.0-only). It does not reproduce
> source code from the original; the `file:line` references point at the
> reference tree in `xara-xtreme/` and serve only to locate the logic being
> described. Xarast is implemented from this specification, not by translating
> the original.

> **Status:** NORMATIVE — freeze candidate for Xarast v0.1.
> **Document:** `docs/research/06-xarast-format.md`
> **Format version described:** `1.0`
> **Date:** 2026-09-19
> **Depends on:** `docs/00-vision-and-scope.md` (principle 3), `research/01-xar-format.md`,
> `research/02-document-model.md`.
> **Associated memory note:** `docs/memory/xarast-format.md` (to be created when the phase starts).

## Normative conventions

This document uses the following keywords with the meaning given in
RFC 2119 / RFC 8174:

| Term | RFC equivalent | Meaning |
|---|---|---|
| **MUST** / **MUST NOT** | MUST / MUST NOT | Absolute requirement. A file or implementation that breaks it is **not conformant**. |
| **SHOULD** / **SHOULD NOT** | SHOULD / SHOULD NOT | Strong recommendation. Deviating requires a documented reason. |
| **MAY** | MAY | Optional; both options are conformant. |

**“Writer”** is used for an implementation that produces `.xarast`, **“reader”** for one
that consumes it, and **“implementation”** for both.

The tag identifiers in parentheses (`TAG_...`) are references to Xara's original binary
format, defined in `/home/user/xara-xtreme/Kernel/cxftags.h`, and provide traceability
between the legacy model and this specification.

---

## Contents

1. [Design goals and non-goals](#1-design-goals-and-non-goals)
2. [Prior research: lessons from other formats](#2-prior-research-lessons-from-other-formats)
3. [Container structure](#3-container-structure)
4. [Compression and deduplication strategy](#4-compression-and-deduplication-strategy)
5. [The Xarast SVG profile](#5-the-xarast-svg-profile)
6. [Complete mapping table: Xara → SVG + `xarast:`](#6-complete-mapping-table)
7. [Document metadata](#7-document-metadata)
8. [Unknown-data preservation (lossless round-trip)](#8-unknown-data-preservation)
9. [Identification: magic bytes, extension, MIME, desktop integration](#9-identification-magic-bytes-extension-mime)
10. [Failure recovery: autosave, journal, locking](#10-failure-recovery)
11. [Formal schemas](#11-formal-schemas)
12. [Complete example of a minimal `.xarast`](#12-complete-example)
13. [Rust implementation plan](#13-rust-implementation-plan)
14. [Appendices](#14-appendices)

---

## 1. Design goals and non-goals

### 1.1 Goals (in decreasing order of priority)

**O1 — Full fidelity within Xarast.** A document saved and reopened in the same version
of Xarast MUST be indistinguishable from the original: same geometry (down to millipoint
precision), same attributes, same *editable* live effects, same undo-equivalent document
state. Saving MUST NOT ever destroy an object's parametricity (a QuickShape remains a
QuickShape, a blend remains a live blend).

**O2 — Graceful degradation outside Xarast.** The `document.svg` file contained in the
package MUST be a **valid, self-contained SVG 1.1 document**, renderable by any modern
browser or by Inkscape with a *visually approximate* result. “Approximate” is defined
measurably in §5.6: SSIM ≥ 0.90 against Xarast's native render for the reference corpus.
Nothing proprietary to Xarast MUST prevent the base render.

**O3 — Space efficiency comparable to `.xar`.** The format MUST apply the same
strategies that made the original `.xar` compact — reuse of bitmap definitions, relative
coordinates, elision of default values, attribute inheritance and stream compression —
translated to the ZIP+SVG domain (§4). Quantitative target: a `.xarast` SHOULD NOT exceed
the equivalent `.xar` by more than **40 %** for predominantly vector documents, and
SHOULD be **equal or smaller** for bitmap-dominated documents (thanks to content-based
deduplication).

**O4 — Lossless round-trip across versions.** A reader of version *N* that opens a file
of version *N+k*, edits it partially and saves it MUST preserve in full the data it does
not understand (§8). This is the property that fails most critically in competing
formats and is a first-class requirement here.

**O5 — Inspectable and repairable with standard tools.** `unzip`, `xmllint`, `git diff`,
`grep` and a text editor MUST suffice to audit, diagnose and — in simple cases — repair a
document. This is a deliberate advantage over opaque binary formats and a guarantee of
archival longevity.

**O6 — Incremental, cheap reading.** Opening the thumbnail, the metadata or the layer
list MUST NOT require decompressing or parsing the whole document. The ZIP central
directory and `META-INF/manifest.xml` MUST suffice.

**O7 — Atomic, failure-resistant writing.** A power cut during saving MUST NOT ever leave
the user without the previous document (§10).

**O8 — Stable under version control.** Two saves with no semantic changes SHOULD produce
byte-identical files (deterministic writing: no varying timestamps in the ZIP unless
explicitly requested, fixed entry order, fixed XML attribute order, stable IDs). This
makes `git` and diffs useful.

### 1.2 Non-goals (explicit)

**N1 — Not a universal interchange format.** The goal is not for other applications to
*edit* `.xarast` with fidelity. For interchange there are plain SVG, PDF and — in the
future — dedicated exporters.

**N2 — No binary `.xar` compatibility on write.** Xarast reads `.xar`; it does not write
it (already declared a non-goal in `00-vision-and-scope.md`). `.xarast` shares not a
single byte of structure with `.xar`.

**N3 — No real-time collaborative editing and no CRDTs in v1.0.** The container reserves
space (`history/`, `META-INF/`) for future extensions, but v1.0 assumes a single writer.

**N4 — No encryption and no digital signatures in v1.0.** The names
`META-INF/encryption.xml` and `META-INF/signatures.xml` are reserved; their semantics are
defined in a later version. A v1.0 reader that encounters these entries MUST reject the
file with a clear error rather than opening it partially.

**N5 — Not a raster image format.** The rasterised document is not stored as the primary
representation; only derived thumbnails and previews.

**N6 — Not “pure” SVG without extensions.** The approach of trying to express everything
in standard SVG is explicitly rejected: it would lose parametricity (O1). Exporting to
plain SVG is a separate, lossy operation.

**N7 — No streaming partial writes over the network.** The file is written in full and
renamed atomically.

---

## 2. Prior research: lessons from other formats

Nine comparable formats have been analysed. This section summarises what is copied and
what is avoided, with the concrete design decision derived from each lesson.

### 2.1 ODF / OpenDocument Graphics (`.odg`) — OASIS

ZIP container with `mimetype` as the **first entry, stored uncompressed and with no
extra field**, so that the MIME type string always falls at fixed offset 38 (30 bytes of
local header + 8 for the name `mimetype`). A `META-INF/manifest.xml` enumerates every
entry with its `media-type`. The content lives in `content.xml`, the styles in
`styles.xml`, the metadata in `meta.xml` and the binaries in `Pictures/`.

- **Copied:** the `mimetype` trick (detection by *magic* without decompressing), the
  explicit manifest, the metadata/content separation, the standard thumbnail.
- **Copied:** the rule that the manifest's `/` entry MUST match the contents of
  `mimetype`.
- **Avoided:** the fragmentation into `content.xml` + `styles.xml` + `settings.xml`,
  which forces cross-references between four XML trees to be resolved in order to paint
  one object. Xarast concentrates the document into a **single** `document.svg`.
- **Avoided:** the bespoke XML vocabulary (`draw:`, `svg:`, `style:`) that *looks* like
  SVG but is not, and therefore cannot be opened in any viewer. That is precisely the
  failure O2 corrects.

### 2.2 Krita (`.kra`)

ZIP with `mimetype`, `maindoc.xml` (layer tree and properties), `documentinfo.xml`,
`preview.png`, `mergedimage.png` and a `layers/` directory holding the binary pixels.

- **Copied:** `mergedimage.png` is an excellent idea — a *flat, always-readable*
  representation of the result, which lets file managers, simple importers and the
  application itself show the document without understanding its model. Xarast adopts
  the vector equivalent: `document.svg` is itself the “merged document”, and there are
  also `thumbnail.png` and `previews/`.
- **Copied:** the radical separation between the tree (small XML, quick to parse) and
  the heavy data (independent binary entries).
- **Avoided:** the opaque per-layer binary (`layers/layerN`), which makes the format
  unauditable and dependent on the exact version of Krita.

### 2.3 Scribus (`.sla`)

Plain XML (optionally gzipped as `.sla.gz`), with a single tree where `<PAGEOBJECT>`
elements are appended in creation order and reference their page by attribute.

- **Avoided:** appearance order decoupled from paint/page order. It is an inexhaustible
  source of bugs and makes it impossible to reason about Z order by reading the file.
  In Xarast **document order IS Z order**, as in SVG.
- **Avoided:** the monolithic XML file with no container: it forces every image to be
  embedded as base64 or depends on external absolute paths (Scribus links images by
  filesystem path: documents break when moved).
  **Derived decision:** in Xarast every referenced resource MUST reside inside the
  package; external absolute references are forbidden (§5.8).
- **Copied:** the readability of the XML and the ease of transforming it with
  XSLT/scripts.

### 2.4 Inkscape SVG (`sodipodi:` / `inkscape:`)

Standard SVG enriched with attributes and elements from two bespoke namespaces
(`http://sodipodi.sourceforge.net/DTD/sodipodi-0.dtd` and
`http://www.inkscape.org/namespaces/inkscape`). Layers as
`<g inkscape:groupmode="layer" inkscape:label="...">`, guides as `<sodipodi:guide>`
inside `<sodipodi:namedview>`, parametric shapes as `<path sodipodi:type="star"
sodipodi:sides="5" ... d="...">`.

- **Copied, and the central inspiration of the design:** the *dual representation*
  technique — the standard attribute (`d`) holds the “baked” geometry that anyone can
  render, and the bespoke-namespace attributes hold the **parameters** that allow it to
  be regenerated. The SVG 1.1 specification guarantees that a user agent “shall include
  unknown attributes in the DOM but shall otherwise ignore them”, which makes this safe
  by construction.
- **Copied:** the deliberate compatibility of layers: emitting `inkscape:groupmode` and
  `inkscape:label` costs ~40 bytes per layer and makes Inkscape open the document with
  its layers intact. It is the highest value/cost ratio in the whole format.
- **Avoided:** the lack of versioning of the bespoke vocabulary. Inkscape does not
  record which version wrote each extension, which has caused historical breakage.
  **Derived decision:** the whole `xarast:` vocabulary is versioned in the namespace URI
  and, in addition, every document declares `xarast:version` and `xarast:min-reader`.
- **Avoided:** the loss of extensions when passing through third-party tools. It is a
  known and well-documented problem that SVG cleaners and parsing libraries strip
  `sodipodi:`/`inkscape:` and leave the file unrecoverable for the editor.
  **Derived decision:** §8 makes preservation a normative obligation and adds a digest
  to *detect* that someone has destroyed the data, instead of failing silently.

### 2.5 Figma (`.fig`)

Binary format based on **Kiwi**, a Protocol Buffers-like binary schema. The document is
serialised as a **flat array of `nodeChanges`** with a reference to the parent, not as a
tree; the tree is reconstructed on load. Each file **embeds its own schema** (more than
500 type definitions), which makes it self-describing.

- **Copied (conceptually):** the idea of self-description. Xarast implements it far more
  cheaply: the file declares which vocabulary version it uses and which capabilities it
  requires of a reader (`xarast:min-reader`, `xarast:requires`).
- **Copied:** the flat model with references is very good for incremental
  synchronisation. It is **not** adopted in v1.0 (N3), but the stable-ID design (§5.7) is
  a precondition for adopting it later without breaking the format.
- **Avoided:** the opaque binary. It contradicts O5 directly. The cost is size and
  parsing speed; this is offset by compression (§4) and by streaming SAX parsing.

### 2.6 SVGZ

Simply SVG compressed with gzip. Widely supported (browsers decompress it if the server
sends `Content-Encoding`, and Inkscape/libxml2 do it on the fly).

- **Avoided as a native format:** a single gzip stream allows no random access, no cheap
  thumbnail (O6), no storing binaries without recompressing them, and no deduplication.
  It would force base64 for every image: **+33 % size** on already-compressed data.
- **Kept as an export format:** Xarast SHOULD offer “Export → compressed SVG (`.svgz`)”,
  producing a single self-contained file with `data:` URIs.

### 2.7 Adobe Illustrator (modern `.ai`)

A valid PDF with Illustrator's proprietary data embedded in a `PieceInfo`/`AIPrivateData`
stream inside the PDF itself.

- **Copied:** exactly the same principle as O2/§5.1 — a standard, widely readable format
  with the rich data attached on a channel that standard readers ignore. It is the
  real-world validation that the strategy works at industrial scale over decades.
- **Avoided:** the “private channel” being an opaque binary blob. In `.ai`, if
  Illustrator cannot read its `AIPrivateData`, the user is left with a flat,
  non-editable PDF, and there is no way to diagnose it. In Xarast the private channel is
  readable XML.
- **Avoided:** the total duplication of geometry. `.ai` stores the artwork twice (PDF +
  private), almost doubling the size. Xarast **does not duplicate geometry** when
  standard SVG suffices; it only bakes alternative representations for what SVG cannot
  express, and those baked subtrees are marked with `xarast:generated` and **can be
  discarded and regenerated** (§5.4).

### 2.8 Affinity (`.afdesign`, `.afphoto`, `.afpub`)

Proprietary, undocumented binary format (header starting with `00 FF 4B 41`).

- **Everything avoided.** It is cited as a counter-example: no external tool reads it,
  there is no possible recovery of damaged files, and interoperability depends entirely
  on the vendor. It is the scenario Xarast exists in order not to repeat.

### 2.9 Xara's own `.xar`

A format of `(tag: u32, size: u32, data)` records with two stages of compression: “rich”
data types that encode high-level graphical information in very few bytes, and zlib
compression of the record stream between `TAG_STARTCOMPRESSION` (30) and
`TAG_ENDCOMPRESSION` (31). There are four concrete efficiency lessons, and all four are
carried over in §4.5:

1. **Shared definitions referenced by index.** `TAG_DEFINEBITMAP_*` (65-71) defines a
   bitmap once; each `TAG_NODE_BITMAP` (198) node references it. The same goes for
   `TAG_DEFINERGBCOLOUR` (50) and `TAG_DEFINECOMPLEXCOLOUR` (51).
2. **Relative coordinates.** `TAG_PATH_RELATIVE*` (113-116) and `TAG_PATHREF_TRANSLATE`
   (4014) / `TAG_PATHREF_IDENTICAL` (4013) avoid repeating absolute coordinates.
3. **“Simple” vs “complex” variants of the same record.** `TAG_ELLIPSE_SIMPLE` (1000) vs
   `TAG_ELLIPSE_COMPLEX` (1001), `TAG_RECTANGLE_SIMPLE` (1100) vs the 16
   rounded/starred/reshaped variants: the frequent case does not pay for the rare case's
   fields. Also `TAG_FLATFILL_NONE/BLACK/WHITE` (190-192): 0-byte records for the most
   common values.
4. **Attribute inheritance by position in the tree.** Attributes apply to the following
   subtree (`TAG_UP`/`TAG_DOWN`), so they are not repeated per object. This is exactly
   the inheritance semantics of SVG presentation properties on `<g>`.

- **Avoided:** the global tag registry frozen into numeric ranges (`0-3999`
  Camelot 1.5, `4000-4999` Camelot 2.0), which forced comments of the kind
  “*DO NOT define tags outside this range. Ask Mark Neves*”. XML namespaces eliminate the
  problem of centralised identifier allocation.

### 2.10 Synthesis: the five structural decisions

| # | Decision | Derived from |
|---|---|---|
| D1 | **ZIP** container with `mimetype` STORED in first position | ODF, KRA, EPUB |
| D2 | A **single** `document.svg`, valid SVG 1.1, with Z order = document order | Against ODF/Scribus; in favour of SVG |
| D3 | **Dual representation**: baked standard SVG + `xarast:` parameters | Inkscape, `.ai` |
| D4 | **Content-based deduplication** (BLAKE3) and aggressive elision in the SVG serialiser | `.xar` |
| D5 | **Mandatory preservation** of the unknown, with a verification digest | Against Inkscape/everyone |

---

## 3. Container structure

### 3.1 Base format

A `.xarast` file **MUST** be a ZIP archive conforming to *PKWARE APPNOTE 6.3.x*
(compatible with the subset implemented by `zip`/`unzip`, `libarchive`, `libzip`,
`java.util.zip` and Rust's `zip` crate).

- The archive **MUST** use the classic “End of Central Directory” where it fits; it
  **MUST** use ZIP64 when the archive exceeds 4 GiB or 65,535 entries, and readers
  **MUST** support ZIP64.
- Entry names **MUST** be encoded in **UTF-8** with bit 11 of the general purpose flag
  (*language encoding flag*, EFS) set.
- Names **MUST** use `/` as the separator, **MUST NOT** begin with `/`, and **MUST NOT**
  contain `..`, `\`, control characters or invalid UTF-8 sequences. A reader **MUST**
  reject (not silently sanitise) any entry that breaks this: it is the *zip-slip*
  mitigation.
- The archive **MUST NOT** contain duplicate entries. A reader that finds them **MUST**
  reject the file.
- Directory entries (name ending in `/`, size 0) **MAY** be omitted; readers **MUST
  NOT** depend on their existence.
- ZIP encryption **MUST NOT** be used in v1.0 (N4).

### 3.2 Normative entry layout

The order of entries in the archive **MUST** be as follows (absent entries are simply
skipped; the relative order of those present is mandatory):

```
 1. mimetype                          MANDATORY, first, STORED, no extra field
 2. META-INF/manifest.xml             MANDATORY
 3. meta.xml                          MANDATORY
 4. document.svg                      MANDATORY
 5. thumbnail.png                     RECOMMENDED
 6. previews/<...>.png                OPTIONAL
 7. resources/<...>                   OPTIONAL
 8. history/<...>                     OPTIONAL
 9. extensions/<...>                  OPTIONAL (third-party extensions)
10. any other entry                   PRESERVED (§8.3)
```

Within each group, entries **SHOULD** be sorted lexicographically by name (UTF-8 byte
order) to satisfy O8 (determinism).

#### 3.2.1 `mimetype`

- **MUST** be the first entry in the archive.
- **MUST** be stored with method `STORED` (0), uncompressed.
- **MUST NOT** have an *extra* field in the local header (neither for alignment nor for
  extended timestamps): this guarantees that the content starts exactly at **offset 38**
  (30 bytes of local header + 8 bytes for the name `mimetype`).
- Its content **MUST** be exactly the 26 ASCII bytes `application/vnd.xarast+zip`,
  **with no** trailing newline and **no** BOM.

#### 3.2.2 `META-INF/manifest.xml`

Normative inventory of all entries. Details and schema in §3.4 and §11.1.
It **MUST** exist. It **MUST** describe *every* entry in the archive, including
preserved unknown ones (§8.3) and `mimetype` itself.

#### 3.2.3 `meta.xml`

Document metadata (author, dates, units, page size, palette…). §7.
A separate file is chosen — instead of putting everything in the SVG — to satisfy O6:
a file manager can read `mimetype` + `meta.xml` + `thumbnail.png` (typically < 4 KiB
uncompressed) without touching a 20 MiB `document.svg`.

#### 3.2.4 `document.svg`

**The document.** A valid, self-contained SVG 1.1. §5.
- It **MUST NOT** be gzip-compressed *inside* the ZIP (pointless double compression):
  the ZIP already compresses it. The entry is called `.svg`, not `.svgz`.
- It **MUST** begin with the XML declaration `<?xml version="1.0" encoding="UTF-8"?>`.
- It **SHOULD** end with a newline.

#### 3.2.5 `thumbnail.png`

Document thumbnail (first *spread*, page area).
- It **SHOULD** exist in every interactively saved file.
- It **MUST** be a valid PNG, RGBA at 8 bits per channel.
- Its longer side **SHOULD** be **256 px**; it **MUST NOT** exceed 512 px.
- It **MUST** represent the document on a transparent background, with the page colour
  composited underneath if the page has an opaque background.
- It sits at the root (not in `Thumbnails/` as in ODF) for simplicity and because the
  manifest declares its role explicitly.

#### 3.2.6 `previews/`

Per-*spread* previews, for quick navigation in multi-page documents and for the
“versions” dialogue.

```
previews/spread-1.png
previews/spread-2.png
```

- The name **MUST** be `previews/spread-<n>.png`, with `<n>` the 1-based index of the
  spread in document order.
- The longer side **SHOULD** be 512 px and **MUST NOT** exceed 1024 px.
- They are **derived**: a reader **MUST** be able to work without them, and a writer
  **MAY** omit them (e.g. in non-interactive saving or from the CLI with
  `--no-previews`).

#### 3.2.7 `resources/`

All the binaries referenced by the document. Normative subdirectories:

| Subdirectory | Content | Naming |
|---|---|---|
| `resources/images/` | *Master* bitmaps: the original pixels the user imported | `b3-<hash32>.<ext>` |
| `resources/derived/` | Derived renditions (crops, baked photographic adjustments) | `b3-<hash32>.<ext>` |
| `resources/baked/` | Fallback rasterisations for fills/effects that SVG cannot express (§5.4) | `b3-<hash32>.png` |
| `resources/fonts/` | Embedded font subsets (WOFF2) | `b3-<hash32>.woff2` |
| `resources/profiles/` | ICC colour profiles | `b3-<hash32>.icc` |
| `resources/brushes/` | Brush/stroke definitions (`TAG_BRUSHDEFINITION`, 4080) | `b3-<hash32>.xml` |
| `resources/blobs/` | Any other opaque binary (includes preserved data) | `b3-<hash32>.bin` |

- `<hash32>` **MUST** be the **first 32 lowercase hexadecimal characters** of the
  BLAKE3-256 hash of the **uncompressed** content of the resource (128 bits: negligible
  collision risk for this domain). The full 64-hex hash **MUST** appear in the manifest.
- `<ext>` **MUST** correspond to the actual type of the content (`png`, `jpg`, `webp`,
  `avif`, `jxl`, `tiff`, `gif`, `svg`, `woff2`, `icc`, `bin`).
- A writer **MUST NOT** keep entries in `resources/` that are not referenced by the
  document **unless** they are marked as preserved (§8.3) or as history.

#### 3.2.8 `history/` (optional)

Snapshots of earlier versions of the document, for “undo across sessions” and for the
history dialogue.

```
history/index.xml
history/0007/document.svg
history/0007/meta.xml
```

- It **MUST** be disabled by default in v1.0 (enabled by preference).
- `history/index.xml` **MUST** list each snapshot with `id`, `timestamp` (RFC 3339, UTC),
  an optional `label`, and the cumulative size.
- Binary resources **MUST NOT** be duplicated: snapshots reference the same `resources/`
  paths (this is exactly the payoff that makes deduplication worthwhile).
- The writer **MUST** enforce a configurable limit (by default: 10 snapshots or 25 % of
  the document size, whichever is reached first) and prune the oldest ones.

#### 3.2.9 `extensions/`

Space reserved for plugin or third-party application data. A writer **MUST NOT** modify
or remove entries under `extensions/` that it did not create itself (§8.3).

### 3.3 Cross-entry reference rules

- All references from `document.svg` to resources **MUST** be **paths relative to the
  directory of `document.svg` itself**, that is, to the package root
  (`resources/images/b3-....png`).
- References **MUST NOT** be absolute (`/resources/...`), nor `file://`, nor
  `http(s)://`, nor contain `..`. A reader **MUST** treat an external reference as a
  missing resource and **MUST** warn the user rather than load it (mitigation against
  exfiltration and against documents that break when moved, the Scribus failure, §2.3).
- Explicit exception: `data:` URIs **MAY** be used for resources smaller than 4 KiB
  (e.g. a tiny pattern), but **SHOULD NOT** be used above that threshold (§4.3).
- Useful consequence of relative paths: when the package is **unzipped into a
  directory**, `document.svg` opens in a browser (`file://`) and resolves all its images
  correctly. This is the concrete operational route to satisfying O2 (§5.9).

### 3.4 `META-INF/manifest.xml`

Namespace: `https://xarast.org/ns/manifest/1.0`, conventional prefix `mf`.

Example (the complete example is in §12):

```xml
<?xml version="1.0" encoding="UTF-8"?>
<mf:manifest xmlns:mf="https://xarast.org/ns/manifest/1.0"
             mf:version="1.0"
             mf:min-reader="1.0"
             mf:generator="Xarast/0.1.0 (linux; x86_64)"
             mf:profile="portable">
  <mf:file-entry mf:full-path="/" mf:media-type="application/vnd.xarast+zip"/>
  <mf:file-entry mf:full-path="mimetype"
                 mf:media-type="text/plain"
                 mf:role="mimetype"
                 mf:size="26" mf:method="stored"/>
  <mf:file-entry mf:full-path="meta.xml"
                 mf:media-type="application/xml" mf:role="meta"
                 mf:size="1184" mf:method="deflate"
                 mf:digest="blake3-256" mf:digest-value="a3c9...e1"/>
  <mf:file-entry mf:full-path="document.svg"
                 mf:media-type="image/svg+xml" mf:role="document"
                 mf:size="20714" mf:method="deflate"
                 mf:digest="blake3-256" mf:digest-value="7f10...bd"/>
  <mf:file-entry mf:full-path="thumbnail.png"
                 mf:media-type="image/png" mf:role="thumbnail"
                 mf:size="9210" mf:method="stored"/>
  <mf:file-entry mf:full-path="resources/images/b3-4a91c0de5f73b8a2.png"
                 mf:media-type="image/png" mf:role="resource"
                 mf:size="482113" mf:method="stored"
                 mf:digest="blake3-256" mf:digest-value="4a91c0de5f73b8a2...9c"
                 mf:refcount="3"/>
</mf:manifest>
```

Normative rules for the manifest:

1. There **MUST** be exactly one `<mf:file-entry>` with `mf:full-path="/"`; its
   `mf:media-type` **MUST** be identical to the content of the `mimetype` entry (rule
   inherited from ODF).
2. There **MUST** be one `<mf:file-entry>` for **every** ZIP entry (except directory
   entries).
3. `mf:role` **MUST** be one of: `mimetype`, `manifest`, `meta`, `document`,
   `thumbnail`, `preview`, `resource`, `history`, `extension`, `unknown`.
   A reader that encounters an unknown `mf:role` **MUST** treat it as `unknown` and
   preserve the entry.
4. `mf:digest`/`mf:digest-value` **MUST** be present for `document`, `meta` and every
   `resource`; they **MAY** be omitted for `mimetype` and previews.
   The only algorithm defined in v1.0 is `blake3-256` (64 hex, lowercase).
5. `mf:size` is the **uncompressed** size in bytes.
6. `mf:method` is informative (`stored`, `deflate`, `zstd`); the ZIP is the source of
   truth. It serves to audit without opening every local header.
7. `mf:refcount` is informative: the number of references to the resource from
   `document.svg`. Useful for diagnosing how effective deduplication is.
8. `mf:profile` **MUST** be `portable` (only `stored`+`deflate`) or `compact` (allows
   `zstd`). See §4.2.
9. If a reader detects a discrepancy between the manifest and the real ZIP content
   (missing entry, extra entry, or a digest that does not match), it **MUST** warn the
   user and **SHOULD** offer to open in read-only mode. It **MUST NOT** open silently.

### 3.5 Why XML and not JSON for the manifest

`META-INF/manifest.json` was considered. XML is chosen for three concrete reasons:

1. **Homogeneity of the toolchain.** The document is already XML (SVG) and so is the
   metadata. A single parser (`quick-xml`) covers all three; with JSON two would be
   needed, plus two different escaping/encoding models.
2. **Precedent and tooling.** `xmllint --relaxng` validates the manifest in CI without
   writing any code; the ODF/OPC ecosystem has validated this design over 20 years.
3. **Namespaces.** Third-party extensibility with non-collision guarantees is native to
   XML. In JSON a prefix convention has to be invented.

The drawback is acknowledged (verbosity, ~1.6× against JSON) and considered irrelevant:
the typical manifest takes 0.5-3 KiB before compression and < 500 bytes afterwards,
against documents of hundreds of KiB.

---

## 4. Compression and deduplication strategy

### 4.1 Guiding principle

**Do not recompress what is already compressed, compress text aggressively, and do not
store the same thing twice.** The three rules, applied together, reproduce in the
ZIP+SVG domain what `.xar` achieved with rich records and zlib (§2.9).

### 4.2 Permitted compression methods

| ZIP method | ID | `portable` profile | `compact` profile | Use |
|---|---|---|---|---|
| `Stored` | 0 | **MUST** be supported | **MUST** be supported | `mimetype`, already-compressed binaries |
| `Deflate` | 8 | **MUST** be supported | **MUST** be supported | XML, SVG, text |
| `Zstandard` | **93** | **MUST NOT** be used | **MAY** be used | Large XML/SVG, incompressible but not already-compressed binaries |

Normative notes:

- A **conformant reader MUST** support `Stored` and `Deflate`, and **SHOULD** support
  `Zstandard` (method 93).
- A **writer MUST** use the `portable` profile by default. The `compact` profile
  **MUST** be enabled explicitly (preference or `--profile compact`) and **MUST** be
  declared in `mf:profile` and raise `mf:min-reader` to `1.0` with the `zstd`
  capability in `<mf:requires>` (§8.4).
- ID **93** is used and **NOT** 20. ID 20 was assigned to Zstandard in APPNOTE 6.3.7 and
  **replaced by 93** in 6.3.8 to avoid conflicts; 93 is what WinZip, libzip, libarchive,
  7-Zip and Python's `zipfile` module write. A reader **MAY** accept 20 on reading out
  of tolerance, but **MUST NOT** ever write it.
- BZip2, LZMA, XZ, PPMd, Deflate64 and Shrink/Implode **MUST NOT** be used. Reason: poor
  interoperability and marginal gain over zstd.
- Recommended level: `Deflate` level 6 on interactive save, level 9 on
  “Save as… → optimised”; `Zstandard` level 10 interactive, 19 optimised.
  The level does **NOT** affect the conformance of the file.

### 4.3 What to compress and what not (normative table)

| Entry type | Method | Rationale |
|---|---|---|
| `mimetype` | **STORED** (mandatory) | Requirement for fixed-offset detection (§3.2.1) |
| `document.svg`, `meta.xml`, `META-INF/manifest.xml`, `history/**/*.xml`, `*.svg` | **DEFLATE** (or ZSTD in `compact`) | Text: typical ratio 6:1 to 12:1 |
| PNG, JPEG, WebP (lossy and lossless), AVIF, JXL, GIF | **STORED** | They already carry Deflate/DCT/VP8L/AV1 inside. Recompressing costs CPU and saves **nothing**: measured, `deflate -6` over a typical JPEG saves 0.1-0.5 % and over a PNG 0.0-0.2 %. |
| WOFF2 | **STORED** | Internal Brotli |
| ICC | **DEFLATE** | Profiles with repetitive tables: 2:1 to 4:1 |
| Uncompressed TIFF, BMP, RAW | **DEFLATE** (or ZSTD) | Uncompressed pixels. **But**: a writer **SHOULD** convert them to PNG/JPEG on import instead of storing them raw |
| `resources/blobs/*.bin` (preserved opaque) | **DEFLATE** | Unknown: the heuristic in §4.4 decides |
| `thumbnail.png`, `previews/*.png` | **STORED** | PNG |

**Escape rule (mandatory heuristic).** For any entry not explicitly covered above, the
writer **MUST** apply:

1. If the MIME type is in the “already compressed” list → `STORED`.
2. Otherwise, compress the first **64 KiB** with `Deflate` level 1. If the resulting
   ratio is **< 1.05** (less than 5 % saving) → store the whole entry as `STORED`.
   Otherwise → compress it entirely with the profile's method and level.

This avoids the pathological pattern of “naive” ZIPs that spend minutes recompressing
gigabytes of JPEG to save kilobytes.

**On `data:` URIs.** Embedding an image in the SVG as base64 inflates it by **33 %**
*and* puts it inside a stream that does get recompressed, destroying the two previous
optimisations *and* making cross-document deduplication and random access impossible.
That is why §3.3 limits them to 4 KiB.

### 4.4 Content-based deduplication

**Mechanism.** Every binary resource is identified by the **BLAKE3-256 of its
uncompressed content**. BLAKE3 is chosen over SHA-256 for speed (on the order of
1-3 GB/s per core, with internal Merkle-tree parallelisation), which allows hashing on
the fly during import at no perceptible cost, and because it is cryptographically sound
— ruling out adversarial collisions, not just accidental ones.

Rules:

1. On importing a binary, the writer **MUST** compute its BLAKE3-256 and consult the
   document's resource index. If a resource with that hash already exists, it **MUST**
   reuse it and increment its `refcount` instead of adding a new entry.
2. The entry name **MUST** be derived from the hash (§3.2.7), which makes deduplication
   **structural**: it is impossible to have two entries with the same content and
   different names within the same folder.
3. On saving, the writer **MUST** walk the document, count real references and **omit**
   resources with `refcount == 0` (garbage collection), except those referenced by
   `history/` or marked as preserved.
4. The writer **SHOULD** keep an in-memory index `hash → (path, refcount)` while the
   document is open, and **SHOULD NOT** rehash unchanged resources when saving (saving a
   300 MB document must not read 300 MB).

**Second-level deduplication: master + derived.** This is the direct translation of
Xara's `TAG_DEFINEBITMAP_*` + `TAG_XPE_BITMAP_PROPERTIES` (4117) +
`TAG_DEFINEBITMAP_XPE` (4118), where the bitmap is stored once and the processed
variants are regenerated on the fly from the master plus the processing parameters.

- The **master** (the pixels as the user imported them) goes in `resources/images/`.
- Each **derived** rendition (crop, baked brightness/contrast adjustment, rescaled
  screen version) goes in `resources/derived/` and **MUST** declare in the manifest
  `mf:derived-from="<hash of the master>"` and `mf:derivation="<id of the op chain>"`.
- A writer **MAY** omit derived renditions entirely (`--no-derived`) if the operation
  chain is deterministic and described in `<xarast:photo-ops>` (§6.9): the reader
  regenerates them. It is an explicit size/opening-time trade-off.
- Consequence: 8 uses of the same photo with 3 different crops take **1 master +
  3 derived renditions**, not 8 copies.

**Geometry deduplication.** Besides binaries, the writer **SHOULD** deduplicate
identical vector subtrees: if two or more objects have identical geometry and attributes
modulo an affine transformation, it **SHOULD** emit one in `<defs>` and reference it
with `<use>`. This is the SVG equivalent of `TAG_PATHREF_IDENTICAL` (4013) and
`TAG_PATHREF_TRANSLATE` (4014). The recommended threshold is **≥ 3 repetitions** or
**≥ 512 bytes** of repeated `d`, so that the indirection pays off.

### 4.5 The four `.xar` optimisations, translated

| `.xar` technique | Translation in `.xarast` | Measured/estimated saving |
|---|---|---|
| Bitmap definitions referenced by index (`TAG_DEFINEBITMAP_*`) | BLAKE3 deduplication + reused `<pattern>`/`<image>` (§4.4) | Up to **N×** in documents with repetition |
| Relative coordinates (`TAG_PATH_RELATIVE*`) | Relative path commands (`m`,`l`,`c`,`s`,`h`,`v`) + quantisation to 3 decimals | **18-30 %** over absolute `d` |
| “Simple/complex” variants and empty records (`TAG_FLATFILL_BLACK`) | Elision of attributes with SVG default values; presentation attributes instead of `style=`; short IDs | **10-20 %** of the SVG |
| Inheritance by position in the tree (`TAG_UP`/`TAG_DOWN`) | Hoisting of common attributes to the parent `<g>` and use of `<style>` with classes when ≥ 8 objects share paint | **8-25 %** of the SVG |
| Stream compression (`TAG_STARTCOMPRESSION`) | Deflate/zstd of the `document.svg` entry | **6:1 to 12:1** |

#### 4.5.1 Normative passes of the SVG serialiser

The writer **MUST** implement, in this order, the following passes when emitting
`document.svg`. Each one **MUST** be semantically neutral (verifiable by the round-trip
test in §13.5).

1. **Number normalisation.** Coordinates in user units = PostScript points. They are
   emitted with **up to 3 decimals** (exactly 1 millipoint, Xara's internal unit) and
   **without trailing zeros** (`12.5`, not `12.500`). The leading `0` is dropped (`.5`,
   not `0.5`). Exponents only if they shorten the value.
2. **Relative paths.** For each path the absolute and relative variants are generated
   and the shorter is emitted. Repeated commands are collapsed (`l 1,0 l 2,0` →
   `l 1,0 2,0`), `h`/`v` are used for axial segments and `s`/`t` for curves with a
   reflected control point.
3. **Elision of default values.** `fill="black"`, `fill-opacity="1"`, `stroke="none"`,
   `stroke-width="1"`, `opacity="1"`, `stroke-linecap="butt"`,
   `stroke-linejoin="miter"`, `stroke-miterlimit="4"`, `fill-rule="nonzero"`,
   `transform="matrix(1,0,0,1,0,0)"` and `preserveAspectRatio="xMidYMid meet"` are not
   emitted.
   **Exception:** if the SVG default value differs from the value inherited from the
   parent `<g>`, it **MUST** be emitted explicitly.
4. **Attribute hoisting.** If all the direct children of a `<g>` share the same value
   for an inheritable property, it is hoisted to the `<g>` and deleted from the children.
5. **CSS classes.** If ≥ 8 elements share the same set of ≥ 3 paint properties, a rule
   is emitted in a single `<style type="text/css">` inside `<defs>` and they are
   replaced by `class="cN"`. CSS **MUST NOT** be used for anything affecting geometry or
   model semantics (paint only), so that removal of the `<style>` by a third-party tool
   degrades colour, never structure.
6. **Short, stable IDs.** The IDs emitted **MUST** be the model's persistent IDs (§5.7),
   not positional indices. For auto-generated internal elements (gradients, markers,
   clips) short IDs derived from the content hash are used (`g7a3`, `m1f`, `c22`), which
   automatically deduplicates identical definitions.
7. **`<defs>` deduplication.** Two gradients, patterns, markers, filters or `clipPath`
   elements with identical content **MUST** be collapsed into one.
8. **Minimal indentation.** On normal save, no indentation (one newline per top-level
   element). With `--pretty` it is indented by 1 space for git diffs.
   Indentation affects size **before** compressing far more than after (deflate eats
   whitespace almost for free: ~2 % of the compressed size), so it is a user option, not
   a requirement.

### 4.6 Size estimates

Figures estimated over the `xara-xtreme/Designs/` and `testfiles/` corpus, with this
methodology: SVG generated by the 8 passes of §4.5.1, deflate-9, binaries STORED.

#### Case A — Medium vector illustration (≈ 1,800 objects, 14 layers, no bitmaps)

| Component | Uncompressed | Inside the `.xarast` |
|---|---|---|
| `document.svg` | 1,240 KiB | 118 KiB (deflate-9) / 84 KiB (zstd-19) |
| `meta.xml` | 2 KiB | 0.7 KiB |
| `META-INF/manifest.xml` | 1 KiB | 0.4 KiB |
| `thumbnail.png` | 11 KiB | 11 KiB (STORED) |
| `previews/spread-1.png` | 42 KiB | 42 KiB (STORED) |
| ZIP overhead (5 entries × ~110 B) | — | 0.6 KiB |
| **Total** | | **≈ 173 KiB** (portable) / **139 KiB** (compact) |
| Equivalent reference `.xar` | | ≈ 126 KiB |

Ratio against `.xar`: **1.37×** in the portable profile, **1.10×** in compact. Within
goal O3 (≤ 1.40×). The SVG without the 8 optimisation passes would weigh ~2,100 KiB
uncompressed and ~186 KiB compressed: **the passes save 37 % of the final size**.

#### Case B — Photographic document (one 12 MP JPEG, 3.2 MiB, used 8 times with 3 crops)

| Strategy | Size |
|---|---|
| Naive (8 copies embedded as base64, SVG deflate) | ≈ 34.1 MiB |
| Naive (8 copies as ZIP entries, deflate) | ≈ 25.5 MiB |
| 8 copies as ZIP entries, STORED | ≈ 25.6 MiB |
| **`.xarast`: 1 master + 3 derived renditions, all STORED** | **≈ 4.9 MiB** |
| `.xarast` with `--no-derived` (derived renditions regenerated on open) | ≈ 3.3 MiB |

Deduplication contributes a factor of **5.2×** here. This is the case where the format
*beats* the original `.xar`, which deduplicated the master bitmap but stored every
processed XPE variant.

#### Case C — Document with live effects (40-step blend + 3 shadows + 2 contours)

| Component | Note |
|---|---|
| Source geometry (2 blend objects + 5 base objects) | 3 KiB |
| `xarast:` parameters of the effects | 1.2 KiB |
| **Baked subtrees** (40 blend steps + contours as paths) | 96 KiB uncompressed |
| Total `document.svg` | 104 KiB → **9.1 KiB** compressed |

Baking is what dominates the size *before* compression, but it is extremely repetitive
text (40 nearly identical paths) and deflate reduces it ~11:1. **Normative
alternative:** if the baked subtree exceeds **256 KiB** uncompressed, the writer
**SHOULD** replace it with a rasterisation in `resources/baked/` referenced with
`<image>` (§5.4.3), whose size is bounded by the chosen resolution.

#### Case D — Incremental saving with `history/` enabled (10 snapshots)

Since binaries are not duplicated (§3.2.8), the cost of 10 snapshots of a Case A
document is ≈ 10 × 118 KiB = 1.15 MiB, that is **7.8× the base document**. That is why
`history/` is disabled by default and has mandatory pruning. A version 1.1 of the format
**SHOULD** store deltas (`history/0007/document.svg.vcdiff`) instead of full copies; the
entry name is already provided for.

---

## 5. The Xarast SVG profile

### 5.1 The dual-representation principle

**It is the central architectural decision of the format.** Every Xarast object or
attribute is written into `document.svg` as the conjunction of two things:

- **(a) The base representation**: standard, self-sufficient SVG 1.1 that any conformant
  renderer paints reasonably. It may be *exact* (a path with a flat fill) or *baked* (an
  approximation generated by Xarast: 40 paths for a blend, a `<filter>` for a shadow, a
  rasterisation for a fractal fill).
- **(b) The parametric representation**: attributes and elements from the `xarast:`
  namespace, which describe the object *as the Xarast model understands it*, with all
  its editing parameters.

Normative rules of the principle:

1. The base representation **MUST** always exist. There **MUST NOT** ever be an object
   visible in Xarast that is invisible or empty in a standard SVG renderer.
2. The parametric representation, where it exists, is the **source of truth** for
   Xarast. On opening, Xarast **MUST** reconstruct the object from (b) and **MUST**
   discard and regenerate (a).
3. Every SVG subtree that is purely a product of baking **MUST** be marked with
   `xarast:generated="<type>"` on its root element. This tells Xarast “this is derived,
   delete it and recompute it”, and tells an analysis tool “this is not authored
   content”.
4. When (a) and (b) disagree (because a third party edited the SVG), **(b) wins**,
   unless the element carries `xarast:base-authoritative="true"` (§8.5).
5. If an object is expressible **exactly** in standard SVG, the writer **MUST NOT** add
   a redundant parametric representation. A rectangle with a flat fill is written
   `<rect ... fill="#c33"/>` and nothing more. The extension is only paid for when it
   contributes something.

### 5.2 Namespaces

```xml
<svg xmlns="http://www.w3.org/2000/svg"
     xmlns:xlink="http://www.w3.org/1999/xlink"
     xmlns:xarast="https://xarast.org/ns/document/1.0"
     xmlns:inkscape="http://www.inkscape.org/namespaces/inkscape"
     xmlns:sodipodi="http://sodipodi.sourceforge.net/DTD/sodipodi-0.dtd"
     xmlns:dc="http://purl.org/dc/elements/1.1/"
     xmlns:cc="http://creativecommons.org/ns#"
     xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
```

| Prefix | URI | Requirement |
|---|---|---|
| (default) | `http://www.w3.org/2000/svg` | **MUST** |
| `xarast` | `https://xarast.org/ns/document/1.0` | **MUST** |
| `xlink` | `http://www.w3.org/1999/xlink` | **MUST** (SVG 1.1 needs it for `xlink:href`) |
| `inkscape`, `sodipodi` | (see above) | **SHOULD** (interoperability with Inkscape) |
| `dc`, `cc`, `rdf` | (see above) | **SHOULD** (metadata in `<metadata>`) |

- The URI of the `xarast` namespace **MUST** contain the **major** version of the
  vocabulary (`/1.0`). An incompatible version 2 would use
  `https://xarast.org/ns/document/2.0`; compatible additions within the 1.x series do
  **NOT** change the URI.
- The **prefix** is conventional: a reader **MUST** resolve by URI, not by prefix.
- `xlink:href` and `href` (SVG2): the writer **MUST** emit **both** on the elements that
  use them (`<use>`, `<image>`, `<textPath>`, `<mpath>`, gradient references), because
  SVG 1.1 and older renderers only understand `xlink:href` and some modern sanitisers
  only keep `href`. Cost: ~15 bytes per reference, which deflate reduces to almost
  nothing.

### 5.3 SVG subset used

**Base profile: SVG 1.1 Second Edition, Full profile.** The writer **MUST NOT** use
features outside this whitelist without marking them as optional:

**Structure:** `svg`, `g`, `defs`, `symbol`, `use`, `switch`, `title`, `desc`,
`metadata`, `style`.
**Geometry:** `path`, `rect`, `circle`, `ellipse`, `line`, `polyline`, `polygon`.
**Paint:** `linearGradient`, `radialGradient`, `stop`, `pattern`, `marker`,
`solidColor` (SVG2 — only with a fallback).
**Image:** `image`.
**Text:** `text`, `tspan`, `textPath`.
**Clipping and masking:** `clipPath`, `mask`.
**Filters:** `filter` and the primitives `feGaussianBlur`, `feOffset`, `feFlood`,
`feComposite`, `feMerge`, `feMergeNode`, `feBlend`, `feColorMatrix`,
`feComponentTransfer` (+ `feFuncR/G/B/A`), `feTurbulence`, `feDisplacementMap`,
`feSpecularLighting`, `feDiffuseLighting`, `feDistantLight`, `fePointLight`,
`feMorphology`, `feTile`, `feImage`.

**Explicitly forbidden (the writer MUST NOT emit it):**

| Element/mechanism | Reason |
|---|---|
| `<script>`, `on*` | Security: a document must not be able to execute code. A reader **MUST** strip it on opening and warn. |
| `<foreignObject>` | Not predictably renderable; contributes nothing to the model. |
| `<animate>`, `<set>`, `<animateTransform>`, SMIL | Outside the v1.0 model. |
| `<font>`, `<glyph>` (SVG fonts) | Obsolete and unsupported in browsers. Use WOFF2. |
| `<meshgradient>`, `<hatch>` (SVG 2) | Removed from SVG 2 and moved to `svg-next`; no browser implementation. 3/4-colour fills are baked (§6.4). |
| External references (`http(s)://`, `file://`, absolute paths) | §3.3 |
| External XML entities, DTD, `<!ENTITY>` | XXE. The reader **MUST** reject any DOCTYPE with entities. |

**Usable with a mandatory fallback (SVG 2 / CSS Compositing):**

| Feature | Required fallback |
|---|---|
| `style="mix-blend-mode:<mode>"` | The element **MUST** additionally have a plausible `opacity`/colour; the blend mode **MUST** be repeated in `xarast:blend`. Supported by all current browsers, not by all viewers. |
| `style="isolation:isolate"` | Without it the render degrades, but does not break. |
| `href` without `xlink:` | Always accompanied by `xlink:href` (§5.2). |
| `var(--...)` for named colours | Only if `xarast:colour-refs="var"`; by default literals are emitted (§6.12). |
| `paint-order` | Affects the fill/stroke order of text; degrades acceptably. |

### 5.4 Marking baked content

```xml
<g xarast:generated="blend" xarast:generated-by="x:blend-7" xarast:generated-rev="3">
  <!-- 40 intermediate steps, pure standard SVG -->
</g>
```

| Attribute | Mandatory | Meaning |
|---|---|---|
| `xarast:generated` | **MUST** | Generator type: `blend`, `contour`, `shadow`, `bevel`, `mould`, `fill-bake`, `stroke-outline`, `text-outline`, `clone-expand`, `effect` |
| `xarast:generated-by` | **MUST** | ID of the parametric element that produced it |
| `xarast:generated-rev` | **SHOULD** | Monotonic counter; allows stale baking to be detected |
| `xarast:generated-hash` | **MAY** | BLAKE3 of the input parameters; if it does not match on opening, it is regenerated |

Rules:

1. On opening, Xarast **MUST** delete every subtree with `xarast:generated` whose
   `xarast:generated-by` resolves to a parametric element that is present, and
   regenerate it.
2. If `xarast:generated-by` does **not** resolve (the parametric element was deleted by
   a third party), Xarast **MUST** keep the baked subtree as normal editable geometry
   and warn that the live effect has been lost. **Never** delete artwork without a
   replacement.
3. A baked subtree **MUST NOT** contain nested parametric elements: baking is a leaf of
   the editing tree.

#### 5.4.1 Three baking strategies, in order of preference

1. **Equivalent geometry** (preferred): the effect is expressed with SVG
   paths/gradients.
   Applies to: blend, contour, mould, variable stroke, expanded clones.
   *Advantage:* scales losslessly, and is editable if all else fails.
2. **SVG filter** (second choice): `<filter>` with standard primitives.
   Applies to: shadow, feathering, bevel, blurs, photographic adjustments, fractals
   (`feTurbulence` is a remarkably good approximation of Xara's cloud and plasma fills).
   *Advantage:* very compact. *Risk:* the exact result depends on the renderer.
3. **Rasterisation** (last resort): PNG in `resources/baked/` referenced with `<image>`.
   Applies to: high-quality conical/diamond/3-4-colour fills, effects whose geometric
   baking would exceed 256 KiB (§4.6 Case C), and anything new that SVG cannot express.
   The writer **MUST** rasterise at **2× the object's nominal resolution in the
   document**, with a maximum of 4096 px per side, and **MUST** annotate
   `xarast:baked-dpi`.

### 5.5 Coordinate system and units

- **SVG user unit = 1 PostScript point = 1/72 inch.** The point is chosen rather than
  the CSS pixel because Xara's internal unit is the **millipoint** (1/1000 pt,
  `Kernel/doccoord.h`), so that 3 decimals in the SVG represent **exactly** the
  internal precision with no rounding error. The document **MUST** declare it:

```xml
<svg width="210mm" height="297mm" viewBox="0 0 595.276 841.89" ...>
```

  `viewBox` in points, `width`/`height` in the real physical unit, so that a browser and
  a printer get the size right.

- **Y axis:** SVG grows downwards; Xara grows upwards. The document is stored in
  **native SVG convention (Y downwards)**, with the origin at the **top-left corner of
  the spread**. The alternative of applying a global `transform="scale(1,-1)"` is
  explicitly rejected: it breaks text, gradients, filters and readability. The Y
  conversion is done in the importer/exporter, once only.
  Xarast **MUST** save `<xarast:document xarast:y-axis="down">` to make this explicit
  and allow a future `up`.

- **The user's working units** (mm, cm, in, pt, px, and user-defined units with a
  prefix/suffix, `TAG_DEFINE_PREFIXUSERUNIT` 85 / `TAG_DEFINE_SUFFIXUSERUNIT` 86) are a
  **presentation preference**, live in `meta.xml` (§7) and do **NOT** affect the numbers
  in the SVG.

- **Precision.** The writer **MUST** emit coordinates with up to 3 decimals and **MUST
  NOT** emit more. The reader **MUST** accept any valid SVG number, including
  exponential notation.

### 5.6 Measurable criterion for a “reasonable render” (O2)

An automatable conformance test is defined:

1. The document is rendered with Xarast to PNG at 96 dpi → *reference*.
2. `document.svg` is extracted and rendered with **resvg**, with **headless Chromium**
   and with **Inkscape** (`--export-type=png`) → *candidates*.
3. The SSIM of each candidate against the reference is computed.

**Normative thresholds** for the conformance corpus (§13.5):

| Document class | Minimum SSIM | Note |
|---|---|---|
| Geometry only, flat fills and linear/radial gradients | **0.99** | Must be near-exact |
| With transparencies, clips, masks, text | **0.95** | Rasterisation and font differences |
| With live effects (shadow, bevel, feathering, contour, blend) | **0.90** | SVG filters approximate |
| With fractal or conical fills | **0.85** | Baked; more deviation is accepted |
| **Mean over the whole corpus** | **≥ 0.90** | This is the number that closes O2 |

A threshold failure **MUST** break CI.

### 5.7 Object identity

Every object in the model **MUST** have a persistent identifier emitted as the `id`
attribute of the corresponding SVG element.

- The `id` **MUST** be a valid XML name, stable throughout the object's life: it does
  not change on moving, reordering, editing or regrouping. Only a new object gets a new
  `id`.
- Recommended format: `x` + 11 characters of a lowercase base32 alphabet derived from a
  truncated ULID/UUIDv7 (`xk3m9q2vr7t`). It is short (12 bytes), sortable by creation
  time and collides with negligible probability.
- The IDs of generated auxiliary elements (gradients, filters, clips, markers)
  **SHOULD** be derived from the content hash (§4.5.1, pass 6), which makes them both
  deduplicable and stable.
- A reader that finds duplicate `id`s **MUST** reassign the duplicates and warn; a
  document with duplicate `id`s is **not conformant**.
- IDs are the foundation on which incremental synchronisation and collaborative editing
  will be built in the future (§2.5, N3).

### 5.8 Document tree structure

```xml
<svg …>                                        <!-- document -->
  <title>…</title>
  <desc>…</desc>
  <metadata>  <rdf:RDF>…</rdf:RDF>  </metadata>
  <defs>
    <style type="text/css">…</style>           <!-- paint classes (§4.5.1) -->
    <xarast:document …/>                       <!-- global properties -->
    <xarast:palette …>…</xarast:palette>       <!-- named colours -->
    <xarast:units …/>                          <!-- user units -->
    <!-- gradients, patterns, filters, markers, clipPaths, symbols -->
  </defs>
  <sodipodi:namedview …>                       <!-- Inkscape-compatible guides/grid -->
    <sodipodi:guide …/>
  </sodipodi:namedview>

  <g id="xSPREAD1" xarast:spread="1" xarast:kind="spread" …>
     <xarast:page …/>                          <!-- geometry of the spread's pages -->
     <g id="xL1" inkscape:groupmode="layer" inkscape:label="Background"
        xarast:kind="layer" …>  …objects…  </g>
     <g id="xL2" inkscape:groupmode="layer" inkscape:label="Text"
        xarast:kind="layer" …>  …objects…  </g>
     <g id="xLG" inkscape:groupmode="layer" inkscape:label="Guides"
        xarast:kind="layer" xarast:layer-kind="guide" style="display:none">…</g>
  </g>

  <svg id="xSPREAD2" x="0" y="900" width="595.276" height="841.89"
       viewBox="0 0 595.276 841.89" xarast:spread="2" xarast:kind="spread">
     …
  </svg>
</svg>
```

#### 5.8.1 Pages and spreads

Xara's model is `Document → Chapter → Spread → (Page*, Layer*)`
(`TAG_DOCUMENT` 40, `TAG_CHAPTER` 41, `TAG_SPREAD` 42, `TAG_PAGE` 44, `TAG_LAYER` 43).

**Decision:** all spreads live in **a single `document.svg`**.

- The **first** spread is emitted as a `<g>` directly under the root `<svg>`, at the
  origin of the root `viewBox`. Consequence: **a browser opening `document.svg` shows
  the first page, correctly framed**. This is the desired degradation.
- The **second and subsequent** spreads are emitted as **nested** `<svg>` elements with
  `x`, `y`, `width`, `height` and their own `viewBox`, laid out vertically below the
  first with a gap of one page unit. They fall outside the root `viewBox`, so a browser
  clips them: it sees page 1, not a confusing collage.
- The alternatives are rejected: (i) one SVG per spread in `spreads/` would break direct
  opening in a browser and multiply the shared `<defs>`; (ii) all spreads inside the
  same `viewBox` would produce an illegible initial render.
- **Chapters** (`TAG_CHAPTER`) are represented as a logical grouping in
  `<xarast:document>`, not as a level of the SVG tree, because they have no geometry.
- Each spread **MUST** declare its pages with `<xarast:page>` (position, size, bleed,
  whether it is a double page, spread scaling `TAG_SPREADSCALING_*` 52/53).

For documents with **more than 32 spreads or more than 8 MiB of SVG**, the writer
**MAY** use the split layout (`xarast:layout="split"`): `document.svg` contains the
first spread and a list of `<xarast:spread-ref xarast:href="spreads/spread-N.svgpart"/>`;
the remaining spreads live in `spreads/`. A reader **MUST** support both layouts. The
writer **MUST** annotate `mf:profile` accordingly and **MUST** warn that opening
directly in a browser will only show the first spread.

#### 5.8.2 Layers

A layer is a `<g>` that is a direct child of the spread, **never** nested inside another
layer.

```xml
<g id="xL2"
   inkscape:groupmode="layer" inkscape:label="Text"
   xarast:kind="layer"
   xarast:layer-kind="normal"
   xarast:visible="true"
   xarast:locked="false"
   xarast:printable="true"
   xarast:solid="false"
   xarast:active="true"
   xarast:frame-delay="0"
   style="display:inline;opacity:1">
```

| Attribute | Source in Xara | SVG equivalent | Notes |
|---|---|---|---|
| `inkscape:groupmode="layer"` | — | — | Inkscape compatibility; cost ~28 B |
| `inkscape:label` | `TAG_LAYERDETAILS` (48) | — | Visible name; **MUST** match `xarast:label` if both are present |
| `xarast:visible` | `TAG_LAYERDETAILS` | `style="display:inline|none"` | Both **MUST** be emitted; `display` is the base representation |
| `xarast:locked` | `TAG_LAYERDETAILS` | — | SVG has no locking; only `xarast:` (+ `sodipodi:insensitive="true"` for Inkscape) |
| `xarast:printable` | `TAG_LAYERDETAILS` | — | A non-printable layer **MUST** still be visible on screen |
| `xarast:layer-kind` | `TAG_GUIDELAYERDETAILS` (49) | — | `normal`, `guide`, `background`, `frame` |
| Layer opacity | — | `opacity` on the `<g>` | Xara has no native layer opacity; Xarast adds it and uses standard `opacity` |
| `xarast:frame-*` | `TAG_LAYER_FRAMEPROPS` (4030) | — | Frame animation; outside the scope of v1.0 rendering |

- The **guide layer** **MUST** be emitted with `style="display:none"` so that it does not
  appear in an external viewer, and with `xarast:layer-kind="guide"`.
- **Z order** is document order: the first layer `<g>` is painted first (it ends up at
  the back). This inverts the usual convention of layer panels (where the top layer is
  at the front): the UI **MUST** show the list reversed. It is documented here because
  it is the number one source of bugs in implementations of graphics formats (§2.3).
- The `xarast:solid` attribute corresponds to Xara's concept of a layer with a solid
  background that hides those below it.

### 5.9 How “open it in a browser” is satisfied in practice

It is worth being precise, because there is a real limitation: **a browser does not open
a ZIP**. Requirement O2 is satisfied by four routes, all of them normative:

1. **Extraction.** `unzip documento.xarast -d /tmp/doc && xdg-open /tmp/doc/document.svg`.
   Since resource references are relative to the package (§3.3), everything resolves.
   This is the canonical route and **MUST** always work.
2. **Inkscape.** The same: the extracted `document.svg` is opened. Layers, guides and
   grid appear correctly thanks to `inkscape:groupmode` and `sodipodi:namedview`
   (§5.8.2). Inkscape **preserves** unknown `xarast:` attributes when saving, so the
   Xarast → Inkscape → Xarast round-trip keeps the extensions (§8).
3. **`xarast extract`** (CLI): the Xarast binary **MUST** offer
   `xarast extract doc.xarast -o dir/` and `xarast cat doc.xarast document.svg`.
4. **Export to self-contained plain SVG.** Xarast **MUST** offer “Export → SVG” with
   three variants:
   - `SVG (with extensions)`: `document.svg` as-is, resources as sibling files.
   - `SVG (self-contained)`: resources embedded as `data:` URIs; `xarast:` extensions
     preserved. A single file that opens anywhere.
   - `SVG (plain / Inkscape / web)`: extensions stripped, baking consolidated,
     `<style>` expanded. **Lossy, and labelled as such in the UI.**
   The self-contained variant **MAY** additionally be compressed to `.svgz`.

In addition, the writer **SHOULD** include at the package root a `README.txt` entry
(≈ 700 bytes, deflating to ~350) explaining to the human user what the file is and how
to extract it. Precedent: well-made `.epub` files do this. It is the format's best
investment of 350 bytes.

---

## 6. Complete mapping table

Table notation: **Base** = what is emitted in standard SVG (representation (a));
**Extension** = what is emitted in the `xarast:` namespace (representation (b));
**Loss without (b)** = what degrades in an external viewer.

### 6.1 Geometric objects

| Xara concept | Original tag | SVG base | `xarast:` extension | Loss without (b) |
|---|---|---|---|---|
| Path (fill / stroke / both) | `TAG_PATH*` 100-103, 113-116 | `<path d="…"/>` with `fill`/`stroke` | — (exact) | None |
| Path flags (smooth, rotational, end point) | `TAG_PATH_FLAGS` 111 | — | `xarast:node-flags="s r e …"` (one letter per node) | None visually; node-editing behaviour is lost |
| Group | `TAG_GROUP` 104 | `<g>` | `xarast:kind="group"` | None |
| Group with transparency | `TAG_GROUPTRANSP` 4063 | `<g opacity="…" style="isolation:isolate">` | `xarast:group-transp="true"` | Slightly different compositing |
| Compound group (cached render) | `TAG_COMPOUNDRENDER` 4128, `TAG_CACHEBMP` 4064 | plain `<g>` | `xarast:compound="true"` | None |
| Simple ellipse | `TAG_ELLIPSE_SIMPLE` 1000 | `<ellipse cx cy rx ry/>` | — | None |
| Complex ellipse (rotated/skewed) | `TAG_ELLIPSE_COMPLEX` 1001 | `<ellipse …  transform="matrix(…)"/>` | — | None |
| Simple rectangle | `TAG_RECTANGLE_SIMPLE` 1100 | `<rect x y width height/>` | — | None |
| Rounded rectangle | `TAG_RECTANGLE_SIMPLE_ROUNDED` 1104 | `<rect rx="…"/>` | `xarast:corner-ratio` if the radius is proportional | None |
| Starred / reshaped rectangle and the 12 variants | 1101-1115 | baked `<path d="…">` | `<xarast:quickshape>` (§6.2) | It stops being parametric |
| QuickShape (polygon/star) | `TAG_REGULAR_SHAPE_PHASE_1/2` 1900/1901 | baked `<path d="…">` | `<xarast:quickshape>` (§6.2) | It stops being parametric |
| Dimension line | `TAG_DIMENSION` 4130 | `<g>` with path + text | `<xarast:dimension>` | It stops updating |
| Web address on an object | `TAG_WEBADDRESS` 4020 | `<a xlink:href="…">` wrapping the object | `xarast:web-bbox` | None |

### 6.2 Parametric shapes (QuickShapes)

Xara's `NodeRegularShape` node (`Kernel/nodershp.h`) parameterises rectangles, polygons,
stars and ellipses with a single model. The extension mirrors it field by field:

```xml
<path id="xq4" d="M 120,20 L 149.4,80.6 …Z"
      xarast:shape="quick"
      fill="#e8a33d">
  <xarast:quickshape
      xarast:sides="5"
      xarast:circular="false"
      xarast:stellated="true"
      xarast:primary-curvature="false"
      xarast:stellation-curvature="false"
      xarast:stell-radius-ratio="0.382"
      xarast:primary-curve-ratio="0"
      xarast:stell-curve-ratio="0"
      xarast:stell-offset-ratio="0"
      xarast:centre="120 90"
      xarast:major-axis="0 -70"
      xarast:minor-axis="70 0"
      xarast:matrix="1 0 0 1 0 0"
      xarast:reformed="false"/>
</path>
```

| Attribute | Field in `NodeRegularShape` | Type | Notes |
|---|---|---|---|
| `xarast:sides` | `NumSides` | integer ≥ 3 | Ignored if `circular` |
| `xarast:circular` | `Circular` | bool | The shape is based on a circle |
| `xarast:stellated` | `Stellated` | bool | Star |
| `xarast:primary-curvature` | `PrimaryCurvature` | bool | Curved sides |
| `xarast:stellation-curvature` | `StellationCurvature` | bool | Curved points |
| `xarast:stell-radius-ratio` | `StellRadiusToPrimary` | double | Inner / outer radius |
| `xarast:primary-curve-ratio` | `PrimaryCurveToPrimary` | double | Curvature of the side |
| `xarast:stell-curve-ratio` | `StellCurveToStell` | double | Curvature of the point |
| `xarast:stell-offset-ratio` | `StellOffsetRatio` | double | ±0.5 = 360/N degrees of rotation of the points |
| `xarast:centre` | `UTCentrePoint` | `x y` | In untransformed coordinates |
| `xarast:major-axis`, `xarast:minor-axis` | `UTMajorAxes`, `UTMinorAxes` | `x y` | Vectors from the centre |
| `xarast:matrix` | `TransformMatrix` | 6 doubles | Transformation of the shape |
| `xarast:reformed` | `IsReformed()` | bool | The sides have been edited by hand |
| `<xarast:edge-path>` (child, ×2) | `EdgePath1`, `EdgePath2` | path `d` | **MUST** be emitted only if `reformed` |

Rule: if `xarast:reformed="true"`, the shape has hand-edited sides and the two
`<xarast:edge-path>` elements are mandatory; otherwise the path is regenerated from the
parameters and they **MUST NOT** be emitted.

### 6.3 Fills

All fills additionally carry, where applicable, the common attributes:

| Common attribute | Tag | Values | SVG base |
|---|---|---|---|
| `xarast:fill-repeat` | `TAG_FILL_REPEATING` 163 / `NONREPEATING` 164 / `REPEATINGINVERTED` 165 / `_EXTRA` 206 | `simple`, `repeat`, `reflect`, `repeat-extra` | `spreadMethod="pad|repeat|reflect"` |
| `xarast:fill-profile` | `TAG_BLENDPROFILES` 4072 and ramp profiles | `bias gain` (2 doubles in [-1,1]) | Baked into stops (§6.5) |
| `xarast:fill-effect` | `TAG_FILLEFFECT_FADE` 160 / `RAINBOW` 161 / `ALTRAINBOW` 162 | `fade`, `rainbow`, `alt-rainbow` | HSV interpolation baked into stops |

| Xara fill | Tag | SVG base | Extension | Loss without (b) |
|---|---|---|---|---|
| Flat | `TAG_FLATFILL` 150 (+190-192) | `fill="#rrggbb"` | — | None |
| No fill | `TAG_FLATFILL_NONE` 190 | `fill="none"` | — | None |
| Linear (2 points) | `TAG_LINEARFILL` 153 | `<linearGradient x1 y1 x2 y2 gradientUnits="userSpaceOnUse">` | `xarast:fill="linear"` only if there is a profile | Ramp profile |
| Multi-stage linear | `TAG_LINEARFILLMULTISTAGE` 4075 | Same, with N `<stop>` elements | ditto | Profile |
| 3-point linear | `TAG_LINEARFILL3POINT` 4121, `…MULTISTAGE3POINT` 4122 | `<linearGradient>` + `gradientTransform` reproducing the skew | `xarast:fill="linear3" xarast:p0/p1/p2` | None visually if the skew is affine |
| Circular | `TAG_CIRCULARFILL` 154 | `<radialGradient cx cy r fx=cx fy=cy>` | — | Profile |
| Elliptical | `TAG_ELLIPTICALFILL` 155 | `<radialGradient>` + `gradientTransform="matrix(…)"` | — | Profile |
| Conical | `TAG_CONICALFILL` 156, `…MULTISTAGE` 4078 | **Baked**: a `<g>` of ≤ 96 wedges with flat fills, **or** a `<pattern>` with a rasterised `<image>` | `<xarast:fill xarast:type="conical" xarast:centre xarast:start-angle xarast:end-angle>` + stops | Appears faceted or as a bitmap |
| Diamond / square | `TAG_SQUAREFILL` 200, `…MULTISTAGE` 4088 | **Baked** as for conical, or a `<radialGradient>` with a `gradientTransform` rotated 45° (acceptable approximation) | `<xarast:fill xarast:type="diamond">` | Rounded rather than straight corners |
| 3 colours | `TAG_THREECOLFILL` 202 | **Baked** to an `<image>` (mesh gradients were withdrawn from SVG 2) | `<xarast:fill xarast:type="three-point">` with 3 `<xarast:point x y colour>` | Becomes raster |
| 4 colours | `TAG_FOURCOLFILL` 204 | ditto | `xarast:type="four-point"` | ditto |
| Bitmap | `TAG_BITMAPFILL` 157 | `<pattern patternUnits="userSpaceOnUse" patternTransform="matrix(…)"><image …/></pattern>` | `xarast:fill="bitmap"` + `xarast:tile-mode` | None if it is a simple tiling |
| Contone bitmap (duotone) | `TAG_CONTONEBITMAPFILL` 158 | `<pattern>` + `<filter>` with a luminance `feColorMatrix` + `feComponentTransfer` interpolating start→end | `xarast:contone-start`, `xarast:contone-end` | Slight difference in the tonal curve |
| Fractal — clouds | `TAG_FRACTALFILL` 159 (`FILLSHAPE_CLOUDS` 9) | `<filter>` with `feTurbulence type="fractalNoise"` + `feColorMatrix` + `feComponentTransfer` mapping to the 2 colours, applied to a `<rect>` clipped by the shape | `<xarast:fill xarast:type="fractal-clouds" xarast:seed xarast:graininess xarast:octaves xarast:squash xarast:dpi>` | Texture **similar but not identical**; very acceptable |
| Fractal — plasma | `TAG_FRACTALFILL` 159 (`FILLSHAPE_PLASMA` 10) | ditto with `type="turbulence"` | `xarast:type="fractal-plasma"` | ditto |
| Noise | `TAG_NOISEFILL` 4010 | `feTurbulence` + `feComposite` | `xarast:type="noise"` | ditto |
| Bevel fill | `TAG_BEVEL`-related, `bevfill.cpp` | Part of the bevel filter (§6.8) | — | — |

**Fill rasterisation rule (normative).** When the writer bakes a fill to a bitmap, it
**MUST**:
- rasterise at **twice** the object's nominal resolution (min. 96 dpi, max. 4096 px per
  side);
- store the PNG in `resources/baked/` (deduplicated by hash);
- reference it with a `<pattern>` whose `patternTransform` reproduces the fill geometry
  exactly;
- annotate `xarast:baked-dpi` and `xarast:generated="fill-bake"`.

### 6.4 Non-linear ramp profiles

Xara allows a *bias/gain* curve over the interpolation of any gradient and over the
distribution of a blend or contour. SVG only interpolates linearly between `<stop>`
elements.

**Solution:** baking by adaptive sampling.

```xml
<linearGradient id="g7a3" gradientUnits="userSpaceOnUse" x1="20" y1="20" x2="220" y2="20"
                xarast:profile="0.42 -0.15">
  <stop offset="0"     stop-color="#1c3f8f"/>
  <stop offset=".0625" stop-color="#23499a"/>
  …
  <stop offset="1"     stop-color="#ffd166"/>
</linearGradient>
```

- The writer **MUST** emit `xarast:profile="<bias> <gain>"` (two doubles in [-1, 1];
  `0 0` = linear) and **MUST** omit it when it is linear.
- The writer **MUST** bake the curve by emitting intermediate stops. Number of stops:
  adaptive sampling until the maximum colour error between the real curve and the
  piecewise linear interpolation is **≤ 2/255** in each channel. Minimum **9**, maximum
  **33** stops per span between key colours.
- The reader **MUST** discard the intermediate stops when `xarast:profile` is present
  and recompute them, so as not to accumulate error over successive saves.
- The original key stops are marked with `xarast:key="true"` so they can be
  distinguished from the baked ones; alternatively the writer **MAY** list the key
  colours and their positions in `xarast:stops="0:#1c3f8f 0.5:#7a2ea0 1:#ffd166"`, which
  is more compact and is the **recommended** form.

### 6.5 Transparencies and blend modes

Xara models transparency as a “transparency fill” with the same variety of geometric
shapes as the colour fills (`TAG_*TRANSPARENTFILL`, 166-172, 180-182, 201, 203, 205,
4011), plus a transparency **type** which is in practice a blend mode
(`enum TranspType` in `Kernel/fillval.h`).

#### 6.5.1 Transparency geometry

| Xara transparency | Tag | SVG base | Extension |
|---|---|---|---|
| Flat | `TAG_FLATTRANSPARENTFILL` 166 | `fill-opacity` / `stroke-opacity` / `opacity` | — |
| Linear | `TAG_LINEARTRANSPARENTFILL` 167 | `<mask maskUnits="userSpaceOnUse">` with a `<rect>` painted by a greyscale `<linearGradient>` | `<xarast:transparency xarast:type="linear" …>` |
| Circular / elliptical | 168 / 169 | `<mask>` with a greyscale `<radialGradient>` | `xarast:type="circular|elliptical"` |
| Conical / square / 3-4 col. | 170 / 201 / 203 / 205 | `<mask>` with the same baking as the equivalent fill | ditto |
| Bitmap | `TAG_BITMAPTRANSPARENTFILL` 171 | `<mask>` with a `<pattern>` of the image in greyscale | `xarast:type="bitmap"` |
| Fractal | `TAG_FRACTALTRANSPARENTFILL` 172 | `<mask>` with `feTurbulence` | `xarast:type="fractal-*"` |
| Line transparency | `TAG_LINETRANSPARENCY` 173 | `stroke-opacity` | — |
| 3-point transparency | `TAG_LINEARTRANSPARENTFILL3POINT` 4123 | `<mask>` + `gradientTransform` | `xarast:type="linear3"` |

**Important note on masks.** The luminance value of a `<mask>` in SVG 1.1 is computed
with the linear luminance formula (`linearRGB` by default). The writer **MUST** emit
`color-interpolation="sRGB"` on the `<mask>` and on the gradient so that the result
matches Xara's model (direct alpha, with no gamma conversion). Omitting this produces
visibly different transparencies in a browser. It is a classic mistake.

#### 6.5.2 Transparency types → blend modes

`TranspType` in Xara (`Kernel/fillval.h`) includes: `Mix`, `StainGlass`, `Bleach`,
`Contrast`, `Saturation`, `Darken`, `Lighten`, `Brightness`, `Luminosity`, `Hue`,
`Bevel` and three “special” modes (additive, subtractive, lookup table).

| `TranspType` | Value | SVG/CSS base | Fallback accuracy | Extension |
|---|---|---|---|---|
| `TT_Mix` | 1 | normal alpha (`opacity`) | **Exact** | — (default) |
| `TT_StainGlass` | 2 | `style="mix-blend-mode:multiply"` | **Exact** | `xarast:blend="stained-glass"` |
| `TT_Bleach` | 3 | `style="mix-blend-mode:screen"` | **Exact** | `xarast:blend="bleach"` |
| `TT_DARKEN` | — | `mix-blend-mode:darken` | Exact | `xarast:blend="darken"` |
| `TT_LIGHTEN` | — | `mix-blend-mode:lighten` | Exact | `xarast:blend="lighten"` |
| `TT_SATURATION` | — | `mix-blend-mode:saturation` | Approximate (CSS HSL model vs Xara's HSV) | `xarast:blend="saturation"` |
| `TT_LUMINOSITY` | — | `mix-blend-mode:luminosity` | Approximate | `xarast:blend="luminosity"` |
| `TT_HUE` | — | `mix-blend-mode:hue` | Approximate | `xarast:blend="hue"` |
| `TT_CONTRAST` | — | `<filter>` with `feComponentTransfer type="linear"` around the midpoint | Approximate | `xarast:blend="contrast" xarast:amount="…"` |
| `TT_BRIGHTNESS` | — | `<filter>` with `feComponentTransfer type="linear" intercept` | Approximate | `xarast:blend="brightness"` |
| `TT_BEVEL` | — | Resolved as part of the bevel (§6.8) | — | `xarast:blend="bevel"` |
| Special additive | `T_SPECIAL_1` | `mix-blend-mode:plus-lighter` | Good | `xarast:blend="additive"` |
| Special subtractive | `T_SPECIAL_2` | `mix-blend-mode:multiply` (approx.) | Poor | `xarast:blend="subtractive"` |
| Lookup table | `T_SPECIAL_3` | `<filter>` with `feComponentTransfer type="table"` and the table included | Good | `xarast:blend="lut"` + `<xarast:lut>` |

Rules:

- The writer **MUST** always emit `xarast:blend` when the mode is not `Mix`, even where
  an exact `mix-blend-mode` exists: the canonical name of the mode is Xarast's, and this
  way the reader does not depend on parsing CSS.
- The writer **MUST** emit the `mix-blend-mode` inside `style=` and **NOT** as a
  presentation attribute, because it is a CSS property only.
- Every group containing children with `mix-blend-mode` **SHOULD** carry
  `style="isolation:isolate"` to confine the blending to the group, replicating Xara's
  transparency group semantics (`TAG_GROUPTRANSP` 4063).

### 6.6 Stroke

| Concept | Tag | SVG base | Extension | Loss |
|---|---|---|---|---|
| Line colour | `TAG_LINECOLOUR` 151 (+193-195) | `stroke="#rrggbb"` / `none` | — | — |
| Width | — | `stroke-width` | — | — |
| Dashes | — | `stroke-dasharray`, `stroke-dashoffset` | `xarast:dash-name="…"` (name of the gallery pattern) | The name is lost, not the appearance |
| Caps | — | `stroke-linecap="butt|round|square"` | — | — |
| Joins | — | `stroke-linejoin="miter|round|bevel"`, `stroke-miterlimit` | — | — |
| Stroke scaling with the object | — | — | `xarast:stroke-scales="true"` | The width is not rescaled on transform |
| Arrows / terminators | `arrows.cpp` | `<marker>` + `marker-start`/`marker-end` | `xarast:arrow-start="<name>"`, `xarast:arrow-end`, `xarast:arrow-scale-with-width` | The name and the automatic rescaling are lost |
| Variable-width stroke | `TAG_VARIABLEWIDTHFUNC` 4000, `TAG_VARIABLEWIDTHTABLE` 4001 | **Baked**: a closed `<path>` with `fill` (the stroke outline), `stroke="none"`, `xarast:generated="stroke-outline"` | `<xarast:stroke xarast:type="variable">` + `<xarast:width-table>` (space-separated list of `t:w`) | It stops being editable as a stroke |
| Stroke type / definition | `TAG_STROKETYPE` 4002, `TAG_STROKEDEFINITION` 4003 | — | `xarast:stroke-type="<id>"`, definition in `resources/brushes/` | — |
| Airbrush | `TAG_STROKEAIRBRUSH` 4004 | Baked to an `<image>` or to a filter | `xarast:stroke-type="airbrush"` + parameters | Becomes raster |
| Brush | `TAG_BRUSHATTR` 4079, `TAG_BRUSHDEFINITION` 4080, `TAG_BRUSHDATA` 4081 … 4113 | **Baked**: `<g xarast:generated="stroke-outline">` with the brush instances along the path | `<xarast:brush>` with a reference to `resources/brushes/b3-….xml`, plus the spacing, rotation, scaling, randomness and colour data | It stops being a live brush |
| Tablet pressure | `TAG_BRUSHPRESSUREINFO` 4105, `TAG_BRUSHPRESSUREDATA` 4106, `TAG_BRUSHPRESSURESAMPLEDATA` 4109 | (included in the baking) | `<xarast:pressure>` with the samples in base64 (normalised u16 LE) or a list of decimals | — |
| Line/fill overprint | `TAG_OVERPRINTLINEON` 3500 … 3503 | — | `xarast:overprint-stroke="true"`, `xarast:overprint-fill="true"` | Only affects colour separation |
| Print on all plates | `TAG_PRINTONALLPLATESON` 3504 | — | `xarast:all-plates="true"` | ditto |

Example of a variable-width stroke:

```xml
<g xarast:kind="stroke-variable" id="xv9">
  <xarast:stroke xarast:type="variable" xarast:base-width="4.5"
                 xarast:profile="0.2 0">
    <xarast:width-table>0:0.1 0.15:0.7 0.5:1 0.85:0.7 1:0.1</xarast:width-table>
    <xarast:source-path d="M 10,50 C 60,10 140,90 190,50"/>
  </xarast:stroke>
  <path xarast:generated="stroke-outline" xarast:generated-by="xv9"
        d="M 10,49.8 C …Z" fill="#222"/>
</g>
```

### 6.7 Text

Xara's text model (`TAG_TEXT_STORY_*` 2100-2117, `TAG_TEXT_LINE` 2200, `TAG_TEXT_CHAR`
2202, attributes 2900-2920, and the XaraLX 0.6 extensions: `TAG_TEXT_TAB` 4200,
indentation 4201-4204, linked stories 4205-4207).

| Concept | Tag | SVG base | Extension | Loss |
|---|---|---|---|---|
| Simple text (one line) | `TAG_TEXT_STORY_SIMPLE` 2100 | `<text x y>…</text>` | `xarast:story="simple"` | None |
| Paragraph / area text | `TAG_TEXT_STORY_COMPLEX` 2101 | `<text>` with one `<tspan x y>` **per line**, with baked positions | `<xarast:text-area>` with the containing shape, the indents and the reflow | The text stops reflowing when edited |
| Text on a path | 2110-2117 | Each character placed on the path and turned (`x`, `y`, `rotate` lists, §6.7.1 rule 6); `<textPath>` was the first idea and is not used (T9.5.6: resvg and Inkscape 1.2 cannot draw a multi-run story through it) | `xarast:layout="path"` + `xarast:path-params`, the path as the story's first child (§6.7.1 rule 0) | Reflected or sheared characters stay on straight lines |
| Linked stories | `TAG_TEXT_STORY_LINK_INFO` 4206 | Each story is an independent `<text>` | `xarast:story-next="#idNext"`, `xarast:story-prev` | Reflow between frames is lost |
| Justification | 2902-2905 | `text-anchor="start|middle|end"`; full justification **baked** word by word | `xarast:justify="left|centre|right|full"` | Full justification is frozen |
| Font size | `TAG_TEXT_FONT_SIZE` 2906 | `font-size` (in pt) | — | — |
| Typeface | `TAG_TEXT_FONT_TYPEFACE` 2907 | `font-family="'Name', <fallbacks>"` | `xarast:font-id`, `xarast:font-ref="resources/fonts/b3-….woff2"` | Font substitution |
| Bold / italic | 2908-2911 | `font-weight`, `font-style` | — | — |
| Underline | 2912/2913 | `text-decoration="underline"` | — | — |
| Super/subscript, explicit script | 2914-2917 | `<tspan baseline-shift="…" font-size="…">` | `xarast:script="super|sub|explicit"` + offset and size | — |
| Tracking | `TAG_TEXT_TRACKING` 2918 | Baked into the characters' `x` (§6.7.1) | `xarast:tracking="<thousandths of an em>"` | None |
| Aspect ratio | `TAG_TEXT_ASPECT_RATIO` 2919 | Per-glyph positions baked into the `<tspan>`'s `x="…"` | `xarast:aspect="1.2"` | — |
| Baseline shift | `TAG_TEXT_BASELINE` 2920 | `baseline-shift` or `dy` | — | — |
| Line spacing | 2900 (ratio) / 2901 (absolute) | explicit `y`/`dy` per line | `xarast:line-spacing="ratio:1.2"` or `"abs:14pt"` | It is frozen |
| Manual kerning | `TAG_TEXT_KERN` 2204 | Baked into the characters' `x` (§6.7.1) | `<xarast:kern xarast:em="N"/>` in place | None |
| Tab stops and ruler | `TAG_TEXT_TAB` 4200, `TAG_TEXT_RULER` 4204 | Baked positions | `<xarast:tabs>` and `<xarast:ruler>` | They are frozen |
| Indents | 4201-4203 | Baked into `x` | `xarast:indent-left/first/right` | They are frozen |
| OpenType features | — (Xarast's own `TxtFeatures` attribute, T9.4.9) | The glyphs a browser shapes without them (no `font-feature-settings`) | `xarast:features="liga:0 smcp:1"`: `tag:value` pairs sorted by tag, written only when not empty | The base SVG ignores them |
| Text converted to curves | — | `<path>` | `xarast:was-text="true"` + `<xarast:text-source>` with the original text (accessibility and search) | — |

**Fonts.** Normative rules:

1. The writer **MUST** always record family, weight, style and a stable
   `xarast:font-id`.
2. The writer **SHOULD** embed a **WOFF2 subset** of every font used in
   `resources/fonts/`, and **MUST** emit the corresponding `@font-face` rule in the
   `<style>` inside `<defs>`, so that a browser opening the extracted SVG renders with
   the correct typeface.
3. The writer **MUST** respect the licence: if the font forbids embedding (restrictive
   `fsType` bit in `OS/2`), it **MUST NOT** embed it and **MUST** annotate
   `xarast:font-embed="denied"`.
4. The writer **MUST** emit a generic fallback chain
   (`font-family="'Xara Sans', 'DejaVu Sans', sans-serif"`).
5. In “archival” mode (conformance profile C, §14.2) the writer **MUST** additionally
   emit a copy of the text converted to curves inside a
   `<g xarast:generated="text-outline" style="display:none">`, which the reader can
   enable if the font is unavailable. High cost in size; that is why it is a separate
   profile.

#### 6.7.1 Exact stories (normative for Xarast writers)

The table above is the *intent*; this is what a Xarast writer produces and what a
Xarast reader relies on (XARA-T-0172). A story is written so that a reader rebuilds a
story whose every character **resolves to the same attributes** — the layout and the
render then follow — while the base SVG shows the text where Xarast draws it.

**Structure.**

```xml
<text id="x…" xarast:kind="text" transform="matrix(a -b -c d e f)"
      [xarast:matrix="a b c d"] xml:space="preserve" xarast:exact="true"
      [xarast:layout="column" xarast:width="…" [xarast:word-wrap="false"]]
      [xarast:layout="path" xarast:path-params="REV TAN LEFT RIGHT [REFL ROT SHEAR]"]
      [xarast:auto-kern="false"] …>
  <tspan id="x…" x="…" y="…" [xarast:ruler="36:0 72:2"]>        <!-- one per TextLine -->
    <tspan font-family="'Family', ['Substitute', ]generic" font-size="…" …twins… …paint…>
      [<xarast:fill …/> <xarast:transparency …/> …]            <!-- paint twins, §6.14 -->
      Te<xarast:kern xarast:em="-60"/>xt<xarast:eol/>           <!-- the run's items -->
    </tspan>
    …more runs…
  </tspan>
</text>
```

0. **Text on a path.** `xarast:path-params` holds, space-separated: reversed
   (`true`/`false`), tangential, the left and right indents in points, then — only when
   the story has a pre-fit character transform — reflected, rotation and shear as
   integers (radians in 16.16 fixed point, the `.xar` `ANGLE`). Missing trailing values
   are `false`/0. The path the text follows is the story's first path child, written
   inside the `<g xarast:kind="text-story">` that then wraps the `<text>`. The base SVG
   shows the text along the path (rule 6), not through `<textPath>`.
1. **One line `<tspan>` per `TextLine`**, with the line's id. A line node's own ruler
   (the one a `.xar` line carried) is `xarast:ruler`: `position:kind` pairs, the
   position in points, the kind the format's `type_and_flags` byte.
2. **Runs.** Inside a line, one `<tspan>` per maximal sequence of the line's items whose
   *written* attributes are identical (the text attributes below, the paint and its
   twins). Every item — characters, kerns, breaks — belongs to exactly one run, in
   order. An item's own attribute children apply to it alone. A line with no items has
   exactly one run with no content: it states the **story-level** attribute state around
   the line (its own attribute children out of scope, as the model resolves it).
3. **A run's content**, in item order: a character as text (a tab as U+0009, which
   `xml:space="preserve"` keeps; U+000D as `&#13;`); a character XML cannot carry as
   `<xarast:char xarast:code="HEX"/>`; a manual kern as `<xarast:kern
   xarast:em="N"/>` (thousandths of an em, as stored); a paragraph end as
   `<xarast:eol/>`; a soft line end as `<xarast:eol xarast:soft="true"/>`. Browsers draw
   none of these elements. No whitespace is written between elements inside `<text>`.
4. **A run's text attributes**, always resolved (never inherited from the line or the
   story): `font-family` (the family first, quoted; then the family the application
   substituted, when it did; then a generic family from PANOSE: proportion 9 →
   `monospace`, serif style 11–13 → `sans-serif`, other Latin-text PANOSE → `serif`,
   none → `sans-serif`), `font-size` in points (the drawn size: model size × script
   size when a script is on), `font-weight="bold"`, `font-style="italic"`,
   `text-decoration="underline"`. Twins, each only when it says something:
   `xarast:family` (when the chain cannot spell the family: empty, quotes, commas,
   semicolons, backslashes, outer spaces), `xarast:font` (the full name, when not the
   family), `xarast:panose` (20 hex digits), `xarast:font-substitute` (informative
   only; never read into the model), `xarast:size` (the model size, when `font-size` is
   the scripted size), `xarast:aspect` (shortest round-trip `f32`), `xarast:tracking`
   (thousandths of an em), `xarast:script="on|off OFFSET SIZE"`, `xarast:baseline`
   (points), `xarast:justify="centre|right|full"`, `xarast:line-spacing="ratio:R"` or
   `"abs:PT"`, `xarast:indent-left` / `indent-right` / `indent-first` (points),
   `xarast:ruler`.
5. **A run's paint** is written exactly as an ink element's (§6.3–§6.6, §6.14): fill,
   stroke (text is stroked when the resolved stroke is not `none`), transparencies,
   blend modes, paint twins as child elements before the content. Runs take part in
   pass 5 (classes) but the `<text>` is opaque to pass 4: nothing is hoisted into or
   out of it.
6. **Placement.** With the application's layout available, every run carries an `x`
   list (one value per character the browser draws) and a `y` (one value when the run
   sits on one baseline, a list otherwise), in the story's frame (y down): the left edge
   of the character's cluster box on its baseline, shifts included; a cluster of several
   characters shares its box evenly. `text-anchor` is then never written. **On a path**
   (T9.5.6) the story is laid out as the application draws it (a line as long as the path,
   then carried onto it by arc length) and each run also carries a `rotate` list, one
   value per drawn character: its glyph origin is where the application puts it on the
   path and it turns by the path's angle there (SVG degrees, clockwise); a cluster of
   several characters shares its advance evenly. `rotate` is written only when a value is
   not zero. When the characters are reflected, or sheared by more than a tangent of
   0.005, SVG cannot say it per character and the story stays on straight lines. Without the
   layout, a line `<tspan>` has `x="0"` and a `y` one line height below the previous
   line, and `text-anchor` reflects the line's alignment. Positions are **derived**: a
   reader ignores them.
7. **Matrix.** When six decimals do not reproduce the story matrix's linear part
   exactly, `xarast:matrix` holds `a b c d` of the model (y up) in the shortest
   round-trip spelling. A reader uses it when it agrees with `transform` to 1e-6 and no
   ancestor transform applies; otherwise `transform` wins (an editor changed it).

**Reading.** A reader rebuilds, for each line, a `TextLine` and, run by run, the
attribute nodes whose value differs from the previous run's (the first run of a line
from the story-level state), followed by the run's items. For a content-less single run,
the differing attributes go before the line, at story level, and become the state the
following lines start from. The result resolves every item as the written story did;
where the attributes sat is not preserved (the model's normal form does not depend on
it). A `<text>` without `xarast:exact` is another program's (or a pre-Phase-9 Xarast
file's): lines are `<tspan>`s, runs are their `<tspan>` children, justification comes
from `text-anchor` / `xarast:justify`, line spacing from `y` deltas, and a size that is
not written is the model's default (16 pt).

### 6.8 Live effects

This is the group where the dual-representation principle earns its keep. In every
case: the parametric `<xarast:*>` element lives next to the source object, and the baked
representation lives in a sibling marked with `xarast:generated`.

#### 6.8.1 Shadow (`TAG_SHADOWCONTROLLER` 4050, `TAG_SHADOW` 4051)

```xml
<g id="xs3" xarast:kind="shadow-group">
  <xarast:shadow xarast:type="wall" xarast:blur="6.2" xarast:offset="8 8"
                 xarast:angle="315" xarast:darkness="0.55" xarast:scale="1"
                 xarast:colour="#000000" xarast:penumbra="4"/>
  <g xarast:generated="shadow" xarast:generated-by="xs3"
     filter="url(#fsh3)" opacity="0.55">
    <use xlink:href="#xobj7" href="#xobj7"/>
  </g>
  <g id="xobj7"> <!-- the real object --> </g>
</g>
```

With the filter:

```xml
<filter id="fsh3" x="-30%" y="-30%" width="180%" height="180%"
        color-interpolation-filters="sRGB">
  <feGaussianBlur in="SourceAlpha" stdDeviation="3.1"/>
  <feOffset dx="8" dy="8"/>
  <feFlood flood-color="#000000"/>
  <feComposite in2="SourceAlpha" operator="in"/>
</filter>
```

| Type | `xarast:type` | Baking |
|---|---|---|
| Wall shadow | `wall` | Filter (offset + blur) |
| Floor shadow | `floor` | Copy transformed with perspective/skew + filter. The full `transform` **MUST** be emitted |
| Glow | `glow` | Filter with no offset, with optional `feMorphology` |
| Inner shadow | `inner` | Filter with `feComposite operator="out"` |

`color-interpolation-filters="sRGB"` is **mandatory** on every filter emitted: the SVG
default is `linearRGB`, which produces blurs and shadows visibly different from Xara's.

#### 6.8.2 Bevel (`TAG_BEVEL` 4052, attributes `TAG_BEVATTR_*` 4053-4056, `TAG_BEVELINK` 4057)

```xml
<g id="xv3" xarast:kind="bevel">
  <xarast:bevel xarast:type="round" xarast:indent="6" xarast:direction="outer"
                xarast:light-angle="315" xarast:light-elevation="45"
                xarast:contrast="0.5" xarast:colour="#c0c0c0"/>
  <g xarast:generated="bevel" xarast:generated-by="xv3"/>
  <path id="xv3src" d="…"/>   <!-- source object(s) -->
</g>
```

| Attribute | Meaning |
|---|---|
| `xarast:type` | the cross-section, one of the fifteen names below |
| `xarast:indent` | the band's width, user units (points) |
| `xarast:direction` | `inner` (the band lies inside the object, drawn over it) or `outer` (outside, drawn beneath) |
| `xarast:light-angle` | the light's direction of travel, degrees anticlockwise in the document's frame (y up); the light sits opposite it |
| `xarast:light-elevation` | the light's height above the plane, degrees (the record's tilt; 45 when the record has none) |
| `xarast:contrast` | `0..=1` (the record's 0..100) |
| `xarast:colour` (+ `xarast:colour-ref`) | the bevel node's own flat fill, only when it has one; otherwise the band takes the first colour of the source's first object |

The join is not a bevel attribute: it is the join attribute in force at
the bevel (the controller's or an ancestor's `stroke-linejoin`), as the
original's bevel node finds it applied (`Kernel/nodebev.cpp:845-850`).
`TAG_BEVATTR_*` are read and discarded by the original and are not
written.

`xarast:type` vocabulary: the original's fifteen styles in record order
(`GDraw/gdraw2.h` `eBevelStyle`; the record stores the index). The faces'
cross-sections are drawn by the closed CDraw; Xarast's are its own
(`xarast_geom::bevel`), so a reader without Xarast must treat the
cross-section as a hint.

| Index | `gdraw2.h` | `xarast:type` | Xarast's cross-section |
|---:|---|---|---|
| 0 | `BEVEL_FLAT` | `flat` | a 45° chamfer |
| 1 | `BEVEL_ROUND` | `round` | a quarter circle |
| 2 | `BEVEL_HALFROUND` | `half-round` | half a round |
| 3 | `BEVEL_FRAME` | `frame` | a ridge |
| 4 | `BEVEL_MESA_1` | `mesa` | a chamfer to a plateau over ½ the band |
| 5 | `BEVEL_MESA_2` | `mesa-2` | a chamfer to a plateau over ⅓ |
| 6 | `BEVEL_SMOOTH_1` | `smooth` | a cosine S |
| 7 | `BEVEL_SMOOTH_2` | `smooth-2` | a quintic S |
| 8 | `BEVEL_POINT_1` | `point` | a concave ridge |
| 9 | `BEVEL_POINT_2a` | `point-2a` | two ridges, a |
| 10 | `BEVEL_POINT_2b` | `point-2b` | two ridges, b |
| 11 | `BEVEL_RUFFLE_2a` | `ruffle-2a` | two cosine waves, a |
| 12 | `BEVEL_RUFFLE_2b` | `ruffle-2b` | two cosine waves, b |
| 13 | `BEVEL_RUFFLE_3a` | `ruffle-3a` | three cosine waves, a |
| 14 | `BEVEL_RUFFLE_3b` | `ruffle-3b` | three cosine waves, b |

The earlier draft's `chisel` and `ridge` are not used: no original style
has those names (`frame` is the ridge). A reader **MUST** keep an unknown
name rather than drop the bevel, and a writer **MUST NOT** emit one.

Baking (`xarast:filter="bevel"`, dropped on reading): the silhouette
(grown by a disc of the indent for an outer bevel) blurred by σ = indent/2
into a height field, `feDiffuseLighting` with `feDistantLight
azimuth = 180 − light-angle` and `elevation = light-elevation`, and an
`arithmetic` composite applying the lit-to-flat ratio, scaled by the
contrast, to `SourceGraphic` (inner) or to a flood of the colour
composited beneath the object (outer). It approximates: the cross-section
is the blur's whatever the style, and `feDiffuseLighting` differentiates
per filter pixel, so the lit slope depends on the output resolution.

#### 6.8.3 Contour (`TAG_CONTOURCONTROLLER` 4066, `TAG_CONTOUR` 4067)

```xml
<g id="xc5" xarast:kind="contour">
  <xarast:contour xarast:width="4" xarast:steps="6" xarast:direction="outer"
                  xarast:join="round" xarast:profile="0.3 0"
                  xarast:colour-profile="0 0" xarast:insets="false"/>
  <g xarast:generated="contour" xarast:generated-by="xc5">
    <path d="…"/> <!-- step 6, the outermost, painted first -->
    …
  </g>
  <path id="xc5src" d="…"/>   <!-- source object -->
</g>
```

Baking is mandatory as real offset paths (strategy 1 of §5.4.1). An increasing
`stroke-width` **SHOULD NOT** be used as an approximation: it fails at the joins and on
inner contours.

#### 6.8.4 Blend (`TAG_BLEND` 105, `TAG_BLENDER` 106, `TAG_BLEND_PATH` 4061, `TAG_BLENDPROFILES` 4072, `TAG_BLENDERADDITIONAL` 4073, `TAG_BLENDER_CURVEPROP` 4060, `TAG_BLENDER_CURVEANGLES` 4062, `TAG_NODEBLENDPATH_FILLED` 4074)

```xml
<g id="xb2" xarast:kind="blend">
  <xarast:blend xarast:steps="24"
                xarast:from="#xb2a" xarast:to="#xb2b"
                xarast:position-profile="0.15 0"
                xarast:attribute-profile="0 0"
                xarast:one-to-one="false"
                xarast:antialias="true"
                xarast:path="#xb2path"
                xarast:rotate-along-path="true"
                xarast:start-angle="0" xarast:end-angle="90"
                xarast:tangential="true"/>
  <defs>
    <path id="xb2path" d="M 20,200 C 120,40 260,40 360,200"/>
  </defs>
  <g xarast:generated="blend" xarast:generated-by="xb2">
    <path d="…"/>   <!-- 24 intermediate steps -->
  </g>
  <path id="xb2a" d="…" fill="#e33"/>
  <path id="xb2b" d="…" fill="#33e"/>
</g>
```

- The start and end objects **MUST** be emitted as normal, visible elements: they are
  the user's artwork, not baking.
- The intermediate steps **MUST** go inside a `<g xarast:generated="blend">`.
- For blends with many steps (> 64) the writer **SHOULD** apply the rasterisation rule
  of §4.6 (Case C).
- A “one-to-one” blend between groups (`one-to-one`) **MUST** record the correspondence
  of sub-objects with `<xarast:blend-map>` so that the effect is reconstructed exactly.

#### 6.8.5 Moulds (`TAG_MOULD_ENVELOPE` 107, `TAG_MOULD_PERSPECTIVE` 108, `TAG_MOULD_GROUP` 109, `TAG_MOULD_PATH` 110, `TAG_MOULD_BOUNDS` 4012)

```xml
<g id="xm1" xarast:kind="mould">
  <xarast:mould xarast:type="envelope" xarast:bounds="0 0 200 120">
    <xarast:mould-shape d="M 0,0 C 60,-30 140,30 200,0 L 200,120 C 140,150 60,90 0,120 Z"/>
    <xarast:mould-source>
      <!-- the ORIGINAL, undeformed subtree, so that it can still be edited -->
      <g> <path d="…"/> <text …>…</text> </g>
    </xarast:mould-source>
  </xarast:mould>
  <g xarast:generated="mould" xarast:generated-by="xm1">
    <!-- deformed geometry: paths with the nodes already transformed -->
  </g>
</g>
```

- A mould **MUST** store the **undeformed source** inside `<xarast:mould-source>`: it is
  the only way for the effect to remain reversible. This duplicates that geometry, and
  it is an accepted and bounded cost (the source is usually much simpler than the
  result).
- `<xarast:mould-source>` **MUST NOT** be rendered: it sits inside a foreign-namespace
  element, which SVG renderers ignore entirely. It does not require `display:none`.
- For `xarast:type="perspective"` the baking of a **rectilinear** subtree **MAY** be
  reduced to a single `transform="matrix(…)"` when the perspective degenerates into an
  affine transformation.
- For text under a mould, the baking **MUST** convert the text to curves (there is no
  way to deform `<text>` in SVG); the original text stays in `<xarast:mould-source>`.

#### 6.8.6 Feathering (`TAG_FEATHER` 4086, `TAG_FEATHER_EFFECT` 4127)

```xml
<xarast:feather xarast:width="5" xarast:profile="0 0"/>
```
Baking: a `filter` with `feGaussianBlur` over the alpha channel (a
`feColorMatrix type="matrix"` that isolates alpha) + `feComposite`. The profile is baked
with `feComponentTransfer type="table"` over `feFuncA`.

#### 6.8.7 Generic live effects (`TAG_LIVE_EFFECT` 4125, `TAG_LOCKED_EFFECT` 4126)

Xara delegated these effects to external plugins. In Xarast they are modelled as a
declarative **effect chain**:

```xml
<xarast:effects>
  <xarast:effect xarast:id="blur1" xarast:kind="gaussian-blur" xarast:radius="3"/>
  <xarast:effect xarast:id="lv1" xarast:kind="levels"
                 xarast:black="0.05" xarast:white="0.92" xarast:gamma="1.1"/>
</xarast:effects>
```
- Every known `xarast:kind` **MUST** have a baking to an SVG filter defined in the
  effect registry (§14.3).
- An `xarast:kind` that is **unknown** to the reader **MUST** be preserved (§8) and its
  baking **MUST** be kept as-is (the reader cannot regenerate it, so `xarast:generated`
  is treated as authoritative: `xarast:base-authoritative="true"` is emitted as well).
- A “locked” effect (`TAG_LOCKED_EFFECT`) is marked with `xarast:locked="true"`: its
  baking is never regenerated.

### 6.9 Bitmaps and photography

| Concept | Tag | SVG base | Extension |
|---|---|---|---|
| Bitmap node | `TAG_NODE_BITMAP` 198 | `<image xlink:href="resources/images/b3-….png" x y width height preserveAspectRatio="none" transform="matrix(…)"/>` | `xarast:bitmap-id="b3-…"` |
| Duotone bitmap | `TAG_NODE_CONTONEDBITMAP` 199 | `<image>` + duotone `filter` | `xarast:contone-start/end` |
| Bitmap definition | `TAG_DEFINEBITMAP_*` 65-71, 4138 | Entry in `resources/images/` | `mf:digest` in the manifest |
| JPEG8BPP reconstruction palette | `TAG_DEFINEBITMAP_JPEG8BPP` 71 | — (the JPEG in `resources/images/` is drawn as is) | `xarast:palette="resources/blobs/b3-….bin"` on every element naming the image (§6.9.1) |
| Bitmap properties | `TAG_BITMAP_PROPERTIES` 4115 | — | `<xarast:bitmap-props xarast:dpi xarast:interpolate xarast:transparent-index>` |
| Document smoothing | `TAG_DOCUMENTBITMAPSMOOTHING` 4116 | `image-rendering="auto|pixelated"` | `xarast:bitmap-smoothing` |
| Non-destructive processing (XPE) | `TAG_XPE_BITMAP_PROPERTIES` 4117, `TAG_DEFINEBITMAP_XPE` 4118 | `<image>` to the derived rendition **or** `<image>` to the master + `filter` | `<xarast:photo-ops>` (operation chain) + `mf:derived-from` |
| Embedded sound | `TAG_DEFINESOUND_WAV` 70 | — | `resources/blobs/` + `<xarast:media>` (not played back in v1.0) |

```xml
<image id="xi4" xlink:href="resources/derived/b3-8f2c….jpg" href="resources/derived/b3-8f2c….jpg"
       x="0" y="0" width="240" height="160" preserveAspectRatio="none"
       transform="matrix(1,0,0,1,60,40)"
       xarast:bitmap-id="b3-4a91c0de5f73b8a2">
  <xarast:photo-ops xarast:master="resources/images/b3-4a91c0de5f73b8a2.jpg">
    <xarast:op xarast:kind="crop" xarast:rect="120 80 1800 1200"/>
    <xarast:op xarast:kind="brightness-contrast" xarast:brightness="0.08" xarast:contrast="0.15"/>
    <xarast:op xarast:kind="unsharp" xarast:radius="1.2" xarast:amount="0.4"/>
  </xarast:photo-ops>
</image>
```

This is the mechanism that reproduces Xara's efficiency with bitmaps: **the master is
stored once**, the variants are described with parameters, and a derived rendition is
only materialised when the cost of regenerating it on opening would be high (§4.4).

#### 6.9.1 The JPEG8BPP palette (normative for Xarast writers)

A `.xar` tag-71 bitmap is a JPEG whose decoded colours the original snaps to a palette
of 1–256 entries (`research/01`, XARA-T-0129). The JPEG stays a plain, browser-readable
resource; the palette is Xarast-only data and is stored beside it:

- **Resource.** `resources/blobs/b3-<hash>.bin`, media type
  `application/octet-stream`: the entries in order, **4 bytes each, `r g b a`**, 1–256
  entries (4–1024 bytes). Content-addressed like every resource, so two bitmaps with the
  same palette share one entry.
- **Reference.** `xarast:palette="<package path>"` on **every** element that names the
  image: the `<image>` of a bitmap node, the `<image>` of a bitmap-fill `<pattern>`,
  the `<xarast:transparency>` twin of a bitmap transparency and the `<image>` of its
  mask pattern. Each reference counts one reference for garbage collection.
- **Identity.** A bitmap is the pair *(image, palette)*: the same JPEG with two palettes
  is two bitmaps; without `xarast:palette` it is the plain JPEG.
- **Reader.** A palette that is missing, outside the package, not a whole number of
  entries or longer than 256 is a warning (`DanglingReference`) and the bitmap has no
  palette. A browser ignores the attribute and draws the unsnapped JPEG — a difference
  of a few levels.

#### 6.9.2 What a browser draws for bitmap paint (normative for Xarast writers)

- **Bitmap transparency.** Besides its twin (§6.14 rule 6), a bitmap transparency on an
  element with a box is a `<mask maskUnits="userSpaceOnUse">` over that box (as for a
  graduated transparency, §6.5) holding one `<rect>` filled with the image `<pattern>`
  (the bitmap-fill mapping) through
  `<filter xarast:filter="transparency-mask" color-interpolation-filters="sRGB">` with
  one `feColorMatrix` whose three colour rows are `-.299 -.587 -.114 0 1` and whose
  alpha row is `0 0 0 0 1`: a texel's transparency level is its BT.601 luma (0 opaque …
  255 clear), so the mask is `1 − luma`, alpha ignored. The mask carries no model data;
  a reader takes the twin.
- **Contone bitmap fill.** The pattern's `<image>` has
  `filter="url(#f…)"`, `<filter xarast:filter="contone" color-interpolation-filters="sRGB">`:
  an `feColorMatrix` putting the BT.601 luma into every colour channel (alpha kept), then
  `feComponentTransfer` with `feFuncR/G/B type="table"` sampled from the ramp start →
  end at evenly spaced luma — 2 entries for a fade (exact), 17 for a rainbow effect —
  computed from the key colours **as written** (§6.14 rule 2).
- Both filters are the writer's own: a reader recognises a `<filter>` in `<defs>` by its
  `xarast:filter` value and regenerates it; any other `<filter>` is foreign data (§8).

### 6.10 Clipping and masks

| Concept | Tag | SVG base | Extension |
|---|---|---|---|
| ClipView (live clipping) | `TAG_CLIPVIEWCONTROLLER` 4084, `TAG_CLIPVIEW` 4085, `TAG_CLIPVIEW_PATH` 4137 | `<g clip-path="url(#cpN)">` + `<clipPath id="cpN">` with the path | `xarast:clipview="true"`, `xarast:clip-shape="#…"`, `xarast:clip-keep-shape="true"` |
| Clipping by path (static) | — | `clip-path` | — |
| Mask by transparency | see §6.5 | `mask` | `<xarast:transparency>` |
| Fill rule | — | `fill-rule="nonzero|evenodd"`, `clip-rule` | — |

Xara's ClipView keeps the clipping shape as an editable object. That is why the writer
**MUST** emit the shape **twice**: inside the `<clipPath>` (where it is neither visible
nor editable) and — if `xarast:clip-keep-shape="true"` — as an element with a referenced
`id`, so that Xarast restores it as a first-class object. The second copy **SHOULD** be
implemented with `<use>` from the `<clipPath>` so as not to duplicate the `d`.

### 6.11 Clones, symbols and sets

| Concept | Tag | SVG base | Extension |
|---|---|---|---|
| Live clone (changes with the original) | — | `<use xlink:href="#xorig" transform="…"/>` | `xarast:clone-of="#xorig"`, `xarast:clone-mode="live"` |
| Independent copy | — | Duplicated subtree | — (not a clone) |
| Reusable symbol | — | `<symbol id="…">` in `<defs>` + `<use>` | `xarast:symbol="true"` |
| Clone with its own attributes | — | `<use>` with overriding `fill`/`stroke` | `xarast:clone-override="fill stroke"` |
| Set | `TAG_SETSENTINEL` 4070, `TAG_SETPROPERTY` 4071 | — | `<xarast:set xarast:id="…" xarast:members="#a #b #c">` in `<defs>` |
| Object name (Name Gallery) | `TAG_NAMEGAL_DOCCOMP` 95 | `<title>` inside the element | `xarast:names="button background highlight"` (space-separated list) |
| Duplication offset | `TAG_DUPLICATIONOFFSET` 4124 | — | `xarast:duplicate-offset="10 10"` on `<xarast:document>` |
| Style/template (WizOp) | `TAG_WIZOP` 4040, `TAG_WIZOP_STYLE` 4041, `TAG_WIZOP_STYLEREF` 4042 | — | `<xarast:style-def>` / `xarast:style-ref` |
| Bar property | `TAG_BARPROPERTY` 4087 | — | `<xarast:bar>` |

`<title>` deserves a note: it is the standard, accessible way of naming an object in SVG
(screen readers announce it, browsers show it as a tooltip). The writer **SHOULD** emit
a `<title>` with the object's primary name **in addition to** `xarast:names`, because it
costs little and improves the accessibility of the exported SVG.

### 6.12 Colour: palettes, named colours, CMYK and spot colours

Xara distinguishes **direct RGB** colour (`TAG_DEFINERGBCOLOUR` 50) from **complex**
colour (`TAG_DEFINECOMPLEXCOLOUR` 51), which can be CMYK, HSV, a spot colour, or a
colour **derived** from another (tint, shade, or linked).

#### 6.12.1 The document palette

```xml
<xarast:palette xarast:id="doc">
  <xarast:colour xarast:id="c-sky" xarast:name="Sky"
                 xarast:model="rgb" xarast:srgb="#3388cc"/>
  <xarast:colour xarast:id="c-sky-50" xarast:name="Sky 50%"
                 xarast:model="tint" xarast:parent="#c-sky" xarast:amount="0.5"
                 xarast:srgb="#99c3e5"/>
  <xarast:colour xarast:id="c-corp-red" xarast:name="Corporate red"
                 xarast:model="cmyk" xarast:cmyk="0 0.91 0.76 0.06"
                 xarast:srgb="#d21f35"
                 xarast:profile="resources/profiles/b3-1122….icc"/>
  <xarast:colour xarast:id="c-pantone" xarast:name="PANTONE 485 C"
                 xarast:model="spot" xarast:spot-name="PANTONE 485 C"
                 xarast:cmyk="0 0.95 1 0" xarast:srgb="#da291c"
                 xarast:screen-angle="45" xarast:solid-ink="true"/>
</xarast:palette>
```

| `xarast:model` | Meaning | Mandatory attributes |
|---|---|---|
| `rgb` | Direct sRGB | `xarast:srgb` |
| `cmyk` | CMYK (with or without a profile) | `xarast:cmyk`, `xarast:srgb` |
| `hsv` | HSV | `xarast:hsv`, `xarast:srgb` |
| `grey` | Greyscale | `xarast:grey`, `xarast:srgb` |
| `spot` | Spot colour | `xarast:spot-name`, `xarast:srgb` |
| `tint` | Tint of a parent towards white | `xarast:parent`, `xarast:amount`, `xarast:srgb` |
| `shade` | Shade of a parent towards black | `xarast:parent`, `xarast:amount`, `xarast:srgb` |
| `linked` | Derived by an offset in HSV | `xarast:parent`, `xarast:hsv-delta`, `xarast:srgb` |

**`xarast:srgb` is mandatory in every case**: it is the value that makes the base
representation work, and the one that lets any reader paint something reasonable
without understanding the colour model.

#### 6.12.2 Reference from the objects

Two modes, selectable with `xarast:colour-refs` on `<xarast:document>`:

- **`literal` (default, recommended):**
  ```xml
  <path d="…" fill="#3388cc" xarast:fill-ref="#c-sky"/>
  ```
  Maximum compatibility: any renderer paints the correct colour; Xarast reconstructs the
  link to the palette from `xarast:fill-ref`. Changing the palette colour means
  rewriting every literal, which is cheap and deterministic.

- **`var` (optional):**
  ```xml
  <style>:root{--c-sky:#3388cc;--c-sky-50:#99c3e5}</style>
  …
  <path d="…" fill="var(--c-sky, #3388cc)"/>
  ```
  Modern browsers resolve it; some viewers and Inkscape do so only partially, but the
  fallback value (`, #3388cc`) covers them. It is offered because it makes the file much
  easier to edit by hand, but it is **not** the default mode.

#### 6.12.3 CMYK and ICC profiles

SVG 1.1 defines the `icc-color()` syntax, which renderers that do not support it
**ignore**, keeping the RGB colour that precedes it. It is exactly the degradation
mechanism that is needed:

```xml
<path d="…" fill="#d21f35 icc-color(coated, 0 0.91 0.76 0.06)"
      xarast:fill-ref="#c-corp-red"/>
```
with `<color-profile name="coated" xlink:href="resources/profiles/b3-1122….icc"/>` in
`<defs>`.

- The writer **SHOULD** emit the `icc-color()` syntax when the colour has CMYK
  components and a profile exists.
- The writer **MUST**, in every case, emit `xarast:fill-ref` or the CMYK components in
  `xarast:cmyk` on the element itself if the colour is not in the palette: the
  `icc-color()` form is fragile against sanitisers.
- Colour plates and imagesetting (`TAG_COLOURPLATE` 3508, `TAG_IMAGESETTING` 3507,
  `TAG_PRINTERSETTINGS` 3506/4135, registration marks 3509/3510) are stored in
  `meta.xml` under `<xarast:print>`, not in the SVG: they do not affect the on-screen
  render.

### 6.13 Summary: what is lost in an external viewer

| Looks **identical** | Looks **approximate** | Looks **different but reasonable** |
|---|---|---|
| Paths, shapes, groups, layers, Z order | Shadows, bevels, feathering (SVG filters) | Conical and 3/4-colour fills (faceted or raster) |
| Flat, linear, radial and elliptical fills | Ramp profiles (baked, error ≤ 2/255) | Fractal fills (`feTurbulence` ≠ Xara's fractal) |
| Bitmaps, patterns, clips, masks | HSL blend modes (saturation, hue, luminosity) | Text without the embedded font |
| Flat and graduated transparencies | Contours and blends (exactly baked, but not live) | Overprint, spot colours, CMYK (the sRGB is seen) |
| Strokes, dashes, caps, joins, arrows | Variable strokes and brushes (baked to a fill) | — |
| Text with an embedded font, text on a path | Moulds (exactly baked, not live) | — |

### 6.14 Exact twins: what the base cannot pin (normative for Xarast writers)

A twin exists so that a reload rebuilds **the model's own values**, not an
approximation of them. Rule: *whatever a reader would otherwise have to guess is
written*; whatever is derived from written values (baked stops, a flat approximation)
**MUST** be computed from the values *as written*, so that a reload re-derives the same
bytes (the first re-save of a reloaded document is byte-identical to the save it came
from).

1. **Palette components** (`xarast:components`, a tint's `xarast:amount`, a shade's
   `xarast:shade`) are written in the **shortest decimal that parses back to the same
   `f32`** (no exponent, `.5` for `0.5`) and read with a correctly rounded decimal →
   `f32` conversion. Six decimals do not pin an `f32`; a reader of older files keeps
   the "nudge to `xarast:srgb`" fallback.
2. **Key colours name their palette colour.** Gradient keys (`<stop>`s or
   `xarast:stops`) carry `xarast:stop-refs`, three/four-colour, fractal and noise twins
   `xarast:colour-refs`, a contone bitmap pattern `xarast:contone-refs`: one token per
   key, `#c-N` for an untinted palette colour, `-` for a literal one; omitted when no
   key is a palette colour. A reader takes the palette colour when it resolves to the
   key's written 8-bit value (an editor that changed the colour and left the reference
   gets its new colour), the literal otherwise. A literal key is read as its 8-bit
   value, so the writer derives from that 8-bit value too.
3. **Key positions** in `xarast:stops` / `xarast:levels` use the shortest `f32` form.
   A plain ramp whose positions `<stop offset>`'s four decimals do not pin also gets
   `xarast:stops` (colours) or `xarast:levels` (a transparency mask); `<stop>` elements
   stay the drawing.
4. **A circle's axes.** A radial fill written as `cx cy r` also writes `xarast:minor`
   (when the axes differ), `xarast:aspect-locked="false"` (when unlocked) and
   `xarast:major` whenever a reader would not derive the major axis itself (a quarter
   turn clockwise of the minor axis when that is a radius, `(cx + r, cy)` otherwise).
   The same twins go on a circular transparency's mask gradient; a diamond
   transparency's mask gradient carries `xarast:fill="diamond"` like the fill.
5. **Fill mapping and effect everywhere.** `xarast:fill-repeat` on a bitmap
   `<pattern>` (apart from its own `xarast:tile-mode`), `xarast:repeat` on every
   `<xarast:fill>` / `<xarast:stroke-fill>` / `<xarast:transparency>` twin whose
   mapping is not `none`, and `xarast:fill-effect` on every twin and bitmap pattern
   whose effect is not `fade`.
6. **Transparency twins are complete.** `<xarast:transparency>` records, besides
   `xarast:type` and `xarast:points`: conical — `xarast:levels="0:l … 1:l"`,
   `xarast:profile`, `xarast:ramp-mapping`; three/four-point — `xarast:values="l0 l1
   l2[ l3]"`; fractal/noise — `xarast:values="from to"` and the parameters of the fill
   twin (`xarast:seed`, `graininess`, `gravity`, `squash`, `dpi`, `tileable`,
   `profile`); bitmap — `href`/`xlink:href` to the image resource (one reference for
   garbage collection; never fetched by a browser: the element is not SVG),
   `xarast:tile-mode`, `xarast:dpi`, `xarast:contone="l0 l1"`, `xarast:profile`. The
   keys' blend mode is the side's (below). Levels are 0 (opaque) … 255 (clear).
7. **The stroke's own transparency.** Its twin is `<xarast:stroke-transparency>` (same
   attributes); a graduated one is a `<mask>` in `<defs>` named by
   `xarast:stroke-mask="url(#m…)"` (SVG has no per-stroke mask: a browser draws the
   stroke without it). When the fill is drawn and the stroke's transparency has a blend
   mode, `xarast:stroke-blend` names it and `xarast:blend` is the fill's alone (omitted
   when the fill has none, even though the CSS `mix-blend-mode` — the nearest a browser
   can draw — may then be the stroke's). Without `xarast:stroke-blend`, older rules
   apply: `xarast:blend` is the fill's when there is a fill, the stroke's otherwise; two
   `<xarast:transparency>` twins are fill then stroke; a lone one is the fill's when
   there is a fill.
8. **`meta.xml` statistics** count what was written: `xarast:bitmaps` is the number of
   distinct bitmaps in the package (a bitmap no element uses is not written).
9. **Bitmap palettes.** A bitmap with a reconstruction palette writes
   `xarast:palette` wherever it writes its `href` (§6.9.1); the normal form includes
   the palette's BLAKE3 next to the image's.

---

## 7. Document metadata

### 7.1 Location and authority

The metadata lives in **`meta.xml`**, which is the **authoritative source**. A subset is
**duplicated** in `document.svg` inside `<metadata><rdf:RDF>` in Dublin Core format, so
that the extracted SVG remains self-describing (this is what Inkscape does, and it lets
tools such as `exiftool` read it).

In case of conflict between `meta.xml` and the SVG's `<metadata>`, **`meta.xml` wins**.
The writer **MUST** keep them synchronised when saving.

### 7.2 Contents of `meta.xml`

```xml
<?xml version="1.0" encoding="UTF-8"?>
<xarast:meta xmlns:xarast="https://xarast.org/ns/document/1.0"
             xmlns:dc="http://purl.org/dc/elements/1.1/"
             xarast:version="1.0" xarast:min-reader="1.0">

  <xarast:identity>
    <xarast:doc-id>01J9Q7ZB2K4M8N6P3R5T7V9W1X</xarast:doc-id>
    <xarast:revision>47</xarast:revision>
  </xarast:identity>

  <dc:title>Exhibition poster</dc:title>
  <dc:creator>Ada Lovelace</dc:creator>
  <dc:contributor>Grace Hopper</dc:contributor>
  <dc:description>A3 poster for the spring exhibition.</dc:description>
  <dc:subject>poster, exhibition, spring</dc:subject>
  <dc:language>en-GB</dc:language>
  <dc:rights>CC BY-SA 4.0</dc:rights>

  <xarast:dates>
    <xarast:created>2026-03-11T09:14:02Z</xarast:created>
    <xarast:modified>2026-09-19T17:41:55Z</xarast:modified>
    <xarast:printed>2026-05-02T11:00:00Z</xarast:printed>
    <xarast:editing-duration>PT18H42M</xarast:editing-duration>
    <xarast:editing-cycles>93</xarast:editing-cycles>
  </xarast:dates>

  <xarast:generator xarast:name="Xarast" xarast:version="0.1.0"
                    xarast:platform="linux-x86_64"/>
  <xarast:origin xarast:imported-from="xar" xarast:source-file="cartel.xar"
                 xarast:source-format-version="2.1"/>

  <xarast:statistics xarast:spreads="1" xarast:pages="1" xarast:layers="7"
                     xarast:objects="1842" xarast:bitmaps="3"
                     xarast:fonts="2" xarast:colours="18"/>

  <xarast:units xarast:default="mm" xarast:precision="2">
    <xarast:user-unit xarast:id="u-pica" xarast:name="Pica" xarast:abbrev="pc"
                      xarast:points="12" xarast:prefix="" xarast:suffix=" pc"/>
  </xarast:units>

  <xarast:page-setup xarast:width="297mm" xarast:height="420mm"
                     xarast:orientation="portrait"
                     xarast:bleed="3mm"
                     xarast:double-page="false"
                     xarast:facing="false"
                     xarast:margin-top="10mm" xarast:margin-right="10mm"
                     xarast:margin-bottom="10mm" xarast:margin-left="10mm"/>

  <xarast:grid xarast:kind="rectangular" xarast:origin="0 0"
               xarast:spacing="10mm" xarast:subdivisions="10"
               xarast:visible="true" xarast:snap="true"
               xarast:colour="#c8d8e8"/>

  <xarast:guides>
    <xarast:guide xarast:orientation="vertical"   xarast:position="30mm"  xarast:colour="#00a0ff"/>
    <xarast:guide xarast:orientation="horizontal" xarast:position="45mm"/>
    <xarast:guide xarast:orientation="angled" xarast:position="100mm 100mm"
                  xarast:angle="30"/>
  </xarast:guides>

  <xarast:view xarast:zoom="0.75" xarast:scroll="0 0" xarast:quality="antialiased"
               xarast:active-layer="xL2"/>

  <xarast:nudge xarast:distance="1mm"/>

  <xarast:print>
    <xarast:imagesetting xarast:screen-ruling="150" xarast:screen-angle="45"
                         xarast:dot-shape="round" xarast:negative="false"
                         xarast:emulsion-down="false"/>
    <xarast:plate xarast:name="Cyan"    xarast:enabled="true" xarast:angle="15"/>
    <xarast:plate xarast:name="Magenta" xarast:enabled="true" xarast:angle="75"/>
    <xarast:plate xarast:name="Yellow"  xarast:enabled="true" xarast:angle="0"/>
    <xarast:plate xarast:name="Black"   xarast:enabled="true" xarast:angle="45"/>
    <xarast:printmarks xarast:kind="default"/>
  </xarast:print>

  <xarast:colour-management
      xarast:working-rgb="sRGB IEC61966-2.1"
      xarast:working-cmyk="resources/profiles/b3-1122….icc"
      xarast:rendering-intent="relative-colorimetric"
      xarast:black-point-compensation="true"/>

  <xarast:comment>Revision approved by management on 2026-05-02.</xarast:comment>
</xarast:meta>
```

Correspondence with Xara's tags: `TAG_DOCUMENTCOMMENT` (90), `TAG_DOCUMENTDATES` (91),
`TAG_DOCUMENTFLAGS` (93), `TAG_DOCUMENTINFORMATION` (4136), `TAG_GRIDRULERSETTINGS`
(46), `TAG_GRIDRULERORIGIN` (47), `TAG_DEFINE_DEFAULTUNITS` (87),
`TAG_DEFINE_PREFIXUSERUNIT` (85), `TAG_DEFINE_SUFFIXUSERUNIT` (86),
`TAG_DOCUMENTNUDGE` (4114), `TAG_VIEWPORT` (80), `TAG_VIEWQUALITY` (81),
`TAG_DOCUMENTVIEW` (82), `TAG_SPREADINFORMATION` (45), `TAG_PRINTERSETTINGS` (3506),
`TAG_IMAGESETTING` (3507), `TAG_COLOURPLATE` (3508), `TAG_PRINTMARK*` (3509/3510).

### 7.3 Guides and grid: dual emission

So that Inkscape shows the document's guides, the writer **SHOULD** additionally emit in
`document.svg`:

```xml
<sodipodi:namedview id="base" units="mm" inkscape:document-units="mm"
                    showgrid="true" inkscape:snap-global="true">
  <inkscape:grid type="xygrid" spacingx="10mm" spacingy="10mm" originx="0" originy="0"/>
  <sodipodi:guide position="85,0" orientation="1,0"/>
  <sodipodi:guide position="0,127" orientation="0,1"/>
</sodipodi:namedview>
```

**Mind the Y axis:** Inkscape ≥ 1.0 guides use the Y axis pointing downwards by default,
as Xarast does (§5.5), so the conversion is the identity. The writer **MUST** emit an
`inkscape:document-units` consistent with `xarast:units/@default`.

### 7.4 Format version and capabilities

Three different numbers, with different purposes:

| Field | Where | Semantics |
|---|---|---|
| `xarast:version` | manifest and `meta.xml` | Version of the **format** it was written with (`MAJOR.MINOR`) |
| `xarast:min-reader` | manifest and `meta.xml` | **Minimum** reader version required to open the file **without losing anything** |
| `<mf:requires>` | manifest | List of discrete **capabilities** required |
| `xarast:generator` | `meta.xml` | Application and version that wrote it (diagnostics) |

Compatibility rules:

1. **Backwards (reading old files).** A reader of version *V* **MUST** open without loss
   any file with `xarast:version ≤ V` within the same major version.
2. **Forwards (reading new files).** If `xarast:min-reader > V`, the reader **MUST**
   warn clearly and **SHOULD** offer to open in **read-only mode**. If the user forces
   editing, the rules of §8 apply and the reader **MUST** mark the document as degraded.
3. If `xarast:min-reader ≤ V` but `xarast:version > V`, the reader **MUST** open and
   edit normally: the writer has guaranteed that everything new can be safely ignored.
   **This is the normal route for the evolution of the format.**
4. A writer **MUST** raise `xarast:min-reader` **only** when it introduces something
   whose being ignored would produce a **visually incorrect or dangerous** document,
   never for adding a new parameter to an existing effect.
5. The **major** version is only incremented on an incompatible change to the container
   (a new namespace URI, §5.2).

```xml
<mf:requires>
  <mf:capability mf:name="zstd"/>
  <mf:capability mf:name="mesh-fill-v2" mf:optional="true"/>
</mf:requires>
```
A capability with `mf:optional="true"` does **NOT** prevent opening: it only warns that
something will look worse.

---

## 8. Unknown-data preservation

This is requirement **O4** and is, deliberately, the strictest part of the
specification. The reason is empirical: it is the property that almost every comparable
format fails (§2.4), and its failure turns “the document looks odd” into “the user's
work has been destroyed and there is no way back”.

### 8.1 General principle

> **A reader MUST preserve, in full and in place, every piece of data it does not
> understand, and MUST write it back out when saving.** Not understanding does not
> authorise deleting.

### 8.2 Preservation within `document.svg` and `meta.xml`

The in-memory document model **MUST** have, on each node, a container of *foreign
baggage* that retains:

1. **Unknown foreign-namespace attributes** (including `xarast:` ones from a future
   version): stored as `(uri, local-name, value)` and re-emitted literally on the same
   element.
2. **Unknown foreign-namespace child elements**: stored as the XML subtree **serialised
   as-is** (exact text, including prefixes and any necessary namespace declarations)
   together with its **position** relative to the known siblings, and re-emitted at that
   position.
3. **Unknown SVG elements** (e.g. from unsupported SVG 2): as point 2, but the reader
   **MUST** additionally warn that the document contains SVG it does not understand,
   because it affects the render.
4. **XML comments and processing instructions**: preserved and re-emitted in place.
   (Low cost, high value: people put notes there.)
5. **Unknown standard SVG attributes**: preserved.
6. Attribute order is **NOT** significant and does **NOT** need to be preserved; the
   writer **MUST** emit attributes in a deterministic order (standard SVG first in
   canonical order, then foreign-namespace ones sorted by URI and local name) to satisfy
   O8.

**Example.** Xarast 0.1 opens a document written by Xarast 2.0:

```xml
<path id="xa7" d="M 0,0 L 100,0"
      fill="#c33"
      xarast:shape="quick"
      xarast:mesh-warp="3 0.5 0.2 …"          <!-- unknown in 0.1 -->
      acme:review-state="approved">            <!-- unknown, from a plugin -->
  <xarast:quickshape xarast:sides="5" …/>      <!-- known -->
  <xarast:neural-fill xarast:model="…"/>       <!-- unknown in 0.1 -->
</path>
```

After editing the geometry in 0.1 and saving, the file **MUST** still contain
`xarast:mesh-warp`, `acme:review-state` and `<xarast:neural-fill>`, intact.

#### 8.2.1 Records an importer did not understand: `<xarast:opaque>`

An importer (`.xar` today) keeps a record it does not model as an *opaque node*: the
producer's tag, the record's bytes, and — because a record may open a subtree — the
objects under it. The profile writes it as:

```xml
<xarast:opaque xarast:id="x2p" xarast:tag="4200" xarast:encoding="base64">AAFiaW5h…
  <path id="x2q" d="…" fill="#c33"/>
  <g id="x2r" xarast:kind="group">…</g>
</xarast:opaque>
```

- The payload is the element's text content, base64, **first**; the subtree follows it
  as ordinary profile elements (ids, paint, twins, foreign data), in document order.
  An opaque node without such children is the same element with only the payload.
- The writer **MUST NOT** drop the subtree (it is document content) and **MUST NOT**
  move it out of `<xarast:opaque>`: neither Xarast's renderer nor a browser draws what
  sits under an unknown record (an element in a foreign namespace is never rendered,
  nor its descendants — verified in resvg, Chrome and Inkscape 1.2). The subtree is
  therefore inert in every viewer; it carries no active content, and the reader
  strips any as it does everywhere else.
- Paint inheritance (§4.5.1 passes 4–5) does not cross `<xarast:opaque>`: nothing is
  hoisted into or through it, so every element inside carries its complete paint.
- The reader **MUST** read the element children back as the opaque node's children
  (text between them is not payload unless it is base64: whitespace is skipped).

### 8.3 Preservation of ZIP entries

1. Every ZIP entry whose name does not fit the layout of §3.2 **MUST** be copied
   unaltered on save, together with its manifest entry.
2. The writer **MUST NOT** recompress them with a different method (the compressed
   stream is copied as-is where possible; if not, it is recompressed with the same
   method).
3. Resources under `resources/` referenced **only** from unknown baggage **MUST NOT** be
   garbage-collected. For this, the reader **MUST** scan the baggage for strings
   matching manifest entry paths and mark them as referenced. A deliberately
   conservative rule: we prefer to carry a spare resource than to break a document.
4. `extensions/` and `META-INF/` (except `manifest.xml`) are always preserved.

### 8.4 Loss detection: the preservation digest

Points 1-3 protect against **Xarast**. They do not protect against a third party (an
SVG cleaner, a script, a version of Inkscape that breaks something) that destroys the
baggage. For that:

- The writer **SHOULD** emit on `<xarast:document>`:
  ```xml
  xarast:foreign-digest="blake3:9f2c…"
  xarast:foreign-count="14"
  ```
  where the digest is computed over the canonical concatenation (C14N over each
  fragment, ordered by the path of the owning element) of all the baggage the writer did
  **not** understand.
- On opening, the reader **MUST** recompute the digest. If it does not match or the
  counter has decreased, it **MUST** warn: *“This document has been modified by another
  application and N items of data from a more recent version of Xarast have been lost.
  Saving will overwrite that loss permanently.”* and **SHOULD** offer “Save as a copy”.
- The digest is **NOT** recomputed over the baggage the current reader **does**
  understand: it only covers what is genuinely unknown.

### 8.5 When the user edits an object carrying unknown baggage

This is the hard case and it must be decided explicitly:

1. **An edit that does not touch the object** (moving something else, changing a
   different layer): the baggage is simply preserved.
2. **An edit of orthogonal attributes** (moving the object, changing its colour): the
   baggage is preserved and the object is marked with `xarast:foreign-dirty="true"`.
3. **An edit that invalidates the baggage** (editing the nodes of a path carrying
   `xarast:neural-fill`, when we do not know whether that fill depends on the geometry):
   the reader **MUST** preserve the baggage **and** mark `xarast:foreign-stale="true"`.
   A future reader that understands that baggage **MUST** check the mark and revalidate
   or regenerate rather than trusting it blindly.
4. **Deletion of the object**: the baggage goes with it. No attempt is made to save it.
   It is recorded in the save warning (`N objects with data from future versions
   deleted`).
5. `xarast:base-authoritative="true"` (§5.1, rule 4) is the mark a writer sets when the
   SVG **base** representation must win over the unknown parametric one; it is used,
   for example, by an unknown live effect whose baking we cannot regenerate (§6.8.7).

### 8.6 What a reader must NEVER do

- It **MUST NOT** “normalise” the SVG by rewriting elements it has not touched.
- It **MUST NOT** remove namespace declarations, even if they appear unused: they may be
  in use from inside a serialised baggage fragment.
- It **MUST NOT** reorder children.
- It **MUST NOT** convert `<rect>`/`<circle>` to `<path>` on saving if it has not edited
  them.
- It **MUST NOT** reindent or rewrite the whole document if the user has changed
  nothing (an “open and close” SHOULD NOT produce any writing at all).

### 8.7 Preservation conformance test

Mandatory in CI (§13.5):

1. A reference document is taken and attributes and elements from a fictitious
   namespace (`urn:test:future`) are injected mechanically into every node.
2. It is opened with Xarast, a battery of edits is applied (move, change colour, group,
   change layer, undo, redo) and it is saved.
3. The SVG is extracted and it is checked that **100 %** of the injected content is
   still present, on the same element and in the same relative position.
4. The same is repeated with unknown ZIP entries and with a `meta.xml` containing
   unknown sections.

---

## 9. Identification: magic bytes, extension, MIME

### 9.1 Extension

- Primary extension: **`.xarast`**.
- Alternative extension accepted on reading: `.xrst` (for filesystems that limit the
  extension to 4 characters). The writer **SHOULD NOT** use it by default.
- The writer **MUST NOT** use `.xar` (already taken by Xara and by the macOS xar
  archiver: a double collision, documented in §2).

### 9.2 Signature (magic bytes)

```
offset  0  : 50 4B 03 04                      "PK\x03\x04"  (ZIP local header)
offset 30  : 6D 69 6D 65 74 79 70 65          "mimetype"    (name of the 1st entry)
offset 38  : 61 70 70 6C 69 63 61 74 69 6F 6E 2F 76 6E 64 2E
             78 61 72 61 73 74 2B 7A 69 70    "application/vnd.xarast+zip"  (26 bytes)
```

The complete signature to check is therefore:

```
"PK\x03\x04" at 0  AND  "mimetype" at 30  AND  "application/vnd.xarast+zip" at 38
```

This works **without decompressing anything** and with only 64 bytes read. It is the
same design used by EPUB and ODF, and that is why §3.2.1 forbids the extra field in the
local header of `mimetype`.

### 9.3 MIME type

| Type | Use |
|---|---|
| `application/vnd.xarast+zip` | **Canonical.** The `+zip` suffix (RFC 6839) tells generic tools that the container is a ZIP |
| `application/x-xarast` | Alias tolerated on reading; **MUST NOT** be written |
| `image/svg+xml` | The type of `document.svg` inside the package |

The `vnd.` (vendor) tree is chosen rather than `prs.` (personal) or `x-`
(experimental, nowadays discouraged by RFC 6648). Registration with IANA **SHOULD** be
requested before the stable v1.0.

### 9.4 Linux integration: `shared-mime-info`

File `packaging/linux/xarast.xml` (installed at
`/usr/share/mime/packages/xarast.xml`):

```xml
<?xml version="1.0" encoding="UTF-8"?>
<mime-info xmlns="http://www.freedesktop.org/standards/shared-mime-info">
  <mime-type type="application/vnd.xarast+zip">
    <comment>Xarast document</comment>
    <comment xml:lang="es">Documento de Xarast</comment>
    <comment xml:lang="de">Xarast-Dokument</comment>
    <comment xml:lang="fr">Document Xarast</comment>
    <comment xml:lang="pt">Documento do Xarast</comment>
    <acronym>XARAST</acronym>
    <expanded-acronym>Xarast Vector Document</expanded-acronym>
    <sub-class-of type="application/zip"/>
    <generic-icon name="image-x-generic"/>
    <magic priority="80">
      <match type="string" value="PK\003\004" offset="0">
        <match type="string" value="mimetype" offset="30">
          <match type="string" value="application/vnd.xarast+zip" offset="38"/>
        </match>
      </match>
    </magic>
    <glob pattern="*.xarast"/>
    <glob pattern="*.xrst"/>
    <alias type="application/x-xarast"/>
  </mime-type>
</mime-info>
```

- `priority="80"` is higher than that of `application/zip` (which is low because it is
  generic), following the specification's recommendation to use high values for
  specific subtypes.
- `<sub-class-of type="application/zip"/>` lets file managers offer “Extract here”
  alongside “Open with Xarast”.
- Installation: `update-mime-database /usr/share/mime`.
- For the AppImage, the file **MUST** also be included at the root of the AppDir so that
  desktop integration tools register it.

### 9.5 `.desktop` file

`packaging/linux/org.xarast.Xarast.desktop`:

```ini
[Desktop Entry]
Type=Application
Version=1.5
Name=Xarast
GenericName=Vector Graphics Editor
GenericName[es]=Editor de gráficos vectoriales
Comment=Create and edit vector graphics and photo compositions
Comment[es]=Crea y edita gráficos vectoriales y composiciones fotográficas
Exec=xarast %F
TryExec=xarast
Icon=org.xarast.Xarast
Terminal=false
StartupNotify=true
StartupWMClass=xarast
Categories=Graphics;VectorGraphics;2DGraphics;
Keywords=vector;svg;draw;illustration;xara;
Keywords[es]=vectorial;svg;dibujo;ilustración;xara;
MimeType=application/vnd.xarast+zip;image/svg+xml;image/svg+xml-compressed;application/vnd.xara;application/pdf;image/png;image/jpeg;image/webp;
Actions=NewDocument;

[Desktop Action NewDocument]
Name=New Document
Name[es]=Documento nuevo
Exec=xarast --new
```

`application/vnd.xara` is included because Xarast imports `.xar` (a product
requirement).

### 9.6 Desktop thumbnails

`packaging/linux/xarast.thumbnailer` (in `/usr/share/thumbnailers/`):

```ini
[Thumbnailer Entry]
TryExec=xarast-thumbnailer
Exec=xarast-thumbnailer -s %s %u %o
MimeType=application/vnd.xarast+zip;
```

`xarast-thumbnailer` **MUST** confine itself to extracting `thumbnail.png` from the ZIP
and rescaling it: it must not open or render the document. This is the operational
justification for O6 and for keeping the thumbnail in the package.

### 9.7 Other platforms (planned)

- **Windows:** registry key `.xarast` → `Xarast.Document`, `PerceivedType=image`,
  thumbnail extractor `IThumbnailProvider` that reads `thumbnail.png`.
- **macOS:** `UTExportedTypeDeclarations` with the identifier `org.xarast.document`,
  `UTTypeConformsTo = ["public.zip-archive", "public.composite-content"]`, and a
  Quick Look extension.

---

## 10. Failure recovery

### 10.1 Atomic writing (mandatory)

The writer **MUST** save following exactly this sequence:

1. Create `<name>.xarast.tmp-<pid>-<rand>` **in the same directory** as the target (so
   that the `rename` is atomic: same filesystem).
2. Write the complete ZIP.
3. `flush` + **`fsync`** of the temporary file's descriptor.
4. `rename(tmp, target)` — atomic on POSIX and on Windows (`MoveFileEx` with
   `MOVEFILE_REPLACE_EXISTING`).
5. `fsync` of the containing **directory** (POSIX), so that the rename survives a power
   cut.
6. Delete the associated autosave and journal.

The writer **MUST NOT** truncate or write over the original file at any point. If step 2
or 3 fails, the temporary file is deleted and the original is left intact.

If the target exists and the user has the backup preference enabled, the writer
**SHOULD** keep the previous one as `<name>.xarast.bak` (rotating a single
copy).

### 10.2 Autosave

- Location: **outside** the document, in `$XDG_STATE_HOME/xarast/autosave/<doc-id>/`
  (by default `~/.local/state/xarast/autosave/<doc-id>/`), where `<doc-id>` is the
  document's persistent identifier (`meta.xml` → `<xarast:doc-id>`), or a fresh ULID if
  the document has never been saved.
- Content: a **complete and valid** `.xarast` (not a separate format), called
  `snapshot.xarast`, plus a `state.json` with the path of the original document, the
  `doc-id`, the timestamp, the PID and the host name.
- Cadence: **every 5 minutes** of wall clock **and** every **50 undo operations**,
  whichever comes first. Configurable; `0` disables it.
- Autosave **MUST** use the same atomic writing as §10.1.
- Autosave **MUST NOT** block the interface: it is serialised on a separate thread from
  an immutable snapshot of the model (this is a requirement for the design of the
  document model: persistent structures or copy-on-write make it cheap).
- On startup, Xarast **MUST** scan the autosave directory and, if it finds snapshots
  whose `state.json` points at a PID that no longer exists (or at another host), offer
  recovery with a date comparison against the file on disk.

### 10.3 Operation journal

To reduce the loss window below the autosave cadence:

- Location: `$XDG_STATE_HOME/xarast/autosave/<doc-id>/journal.ndjson`.
- Format: **append-only NDJSON**, one line per applied undo operation, with
  `{"seq":N,"ts":"…","op":"…","payload":{…}}`.
- The writer **MUST** `write` + `fsync` each line (or batch them with an `fsync` every
  250 ms at most).
- On recovery, Xarast loads `snapshot.xarast` and **replays** the journal entries whose
  `seq` is later than the snapshot's.
- The journal **MUST** be truncated on every autosave and deleted on every real save.
- An operation that is not representable in the journal (e.g. a 100 MB import) **MUST**
  force an immediate autosave instead of writing to the journal.
- Putting the journal **inside** the `.xarast` is explicitly rejected: it would force
  the ZIP to be rewritten on every operation.

### 10.4 Lock file

To prevent two instances (or two users on a network drive) from editing the same
document at once:

- Name: `.<name>.xarast.lock`, in the **same directory** as the document (the
  LibreOffice pattern, recognisable and working on network filesystems).
- Content (UTF-8 text, one key per line):
  ```
  xarast-lock/1
  pid=48213
  host=machine-name
  user=jose
  boot-id=8f1c3d2e-…
  doc-id=01J9Q7ZB2K4M8N6P3R5T7V9W1X
  since=2026-09-19T17:02:11Z
  ```
- Creation: with `O_CREAT|O_EXCL` (failing if it exists), plus an advisory
  `flock`/`LOCK_EX|LOCK_NB` lock on the lock file itself, which gives reliable local
  detection even if the process dies.
- If the lock exists:
  - If `host` and `boot-id` match the current ones and the PID is **not** alive → stale
    lock: it is reclaimed automatically and a warning is shown.
  - In any other case → “Open read-only” / “Open a copy” / “Force (risky)” is offered.
- The lock **MUST** be released on closing, and the process **MUST** install handlers
  for `SIGINT`/`SIGTERM` that remove it. It **MUST NOT** be left to `atexit` alone.
- If the directory is read-only, the absence of a lock **MUST NOT** prevent opening.

### 10.5 Reading robustness

- A `.xarast` with a damaged central directory **SHOULD** be recoverable by scanning the
  local headers (`PK\x03\x04`). Xarast **SHOULD** offer `xarast repair doc.xarast`,
  which performs that scan, validates digests against the manifest where it is readable,
  and rebuilds a healthy package.
- A `document.svg` with malformed XML **SHOULD** be recovered by parsing up to the point
  of error and keeping what was read, with a clear warning and read-only opening.
- Entries whose BLAKE3 digest does not match the manifest **MUST** be flagged; the
  document is opened but a warning is shown, and corrupt resources are replaced by a
  visible placeholder.
- Mandatory hard limits against *zip bombs* and malicious XML:
  - maximum decompression ratio per entry: **200:1** (configurable upwards);
  - maximum total uncompressed size: **4 GiB** by default;
  - maximum XML nesting depth: **256**;
  - maximum number of XML entities: **0** (DTDs are rejected, §5.3);
  - maximum number of entries: **65,535** without ZIP64, **1,000,000** with ZIP64.

---

## 11. Formal schemas

They are provided in **RELAX NG Compact** (`.rnc`), being the most readable and the one
that best expresses mixed content and open extensibility
(`anyAttribute`/`anyElement`), which is exactly what §8 requires. The XML versions
(`.rng`) are also generated for `xmllint`.

Location in the repository: `crates/xarast-format/schemas/`.

### 11.1 `manifest.rnc` — `META-INF/manifest.xml`

```rnc
default namespace = ""
namespace mf = "https://xarast.org/ns/manifest/1.0"

start = Manifest

Manifest =
  element mf:manifest {
    attribute mf:version     { xsd:string { pattern = "[0-9]+\.[0-9]+" } },
    attribute mf:min-reader  { xsd:string { pattern = "[0-9]+\.[0-9]+" } },
    attribute mf:generator   { text }?,
    attribute mf:profile     { "portable" | "compact" },
    Requires?,
    FileEntry+,
    AnyForeign*
  }

Requires =
  element mf:requires {
    element mf:capability {
      attribute mf:name     { text },
      attribute mf:optional { xsd:boolean }?
    }+
  }

FileEntry =
  element mf:file-entry {
    attribute mf:full-path   { text },
    attribute mf:media-type  { text },
    attribute mf:role        { Role }?,
    attribute mf:size        { xsd:nonNegativeInteger }?,
    attribute mf:method      { "stored" | "deflate" | "zstd" }?,
    attribute mf:digest      { "blake3-256" }?,
    attribute mf:digest-value{ xsd:string { pattern = "[0-9a-f]{64}" } }?,
    attribute mf:refcount    { xsd:nonNegativeInteger }?,
    attribute mf:derived-from{ xsd:string { pattern = "[0-9a-f]{64}" } }?,
    attribute mf:derivation  { text }?,
    AnyForeignAttr*,
    AnyForeign*
  }

Role = "mimetype" | "manifest" | "meta" | "document" | "thumbnail"
     | "preview" | "resource" | "history" | "extension" | "unknown"

# --- Extensibility: EVERYTHING unknown is valid and MUST be preserved (§8) ---
AnyForeignAttr = attribute * - mf:* { text }
AnyForeign     = element   * - mf:* { (AnyForeignAttr | AnyForeign | text)* }
```

### 11.2 `xarast-doc.rnc` — extensions in `document.svg` (schematic)

**Only** the `xarast:` vocabulary is validated; the base SVG is validated separately
against the official SVG 1.1 schema.

```rnc
namespace x   = "https://xarast.org/ns/document/1.0"
namespace svg = "http://www.w3.org/2000/svg"

Num      = xsd:double
Point    = xsd:string          # "x y"
Matrix6  = xsd:string          # "a b c d e f"
IdRef    = xsd:string          # "#id"
Bool     = xsd:boolean
Colour   = xsd:string          # "#rrggbb" | "#rrggbbaa"
Profile  = xsd:string          # "<bias> <gain>"

# ---------------- Document ----------------
Document =
  element x:document {
    attribute x:version           { text },
    attribute x:min-reader        { text },
    attribute x:y-axis            { "down" | "up" }?,
    attribute x:layout            { "single" | "split" }?,
    attribute x:colour-refs       { "literal" | "var" }?,
    attribute x:duplicate-offset  { Point }?,
    attribute x:foreign-digest    { text }?,
    attribute x:foreign-count     { xsd:nonNegativeInteger }?,
    Chapter*, AnyForeign*
  }

Chapter = element x:chapter { attribute x:name { text }, attribute x:spreads { text } }

Page =
  element x:page {
    attribute x:index  { xsd:positiveInteger },
    attribute x:rect   { xsd:string },      # "x y w h"
    attribute x:bleed  { Num }?,
    attribute x:double { Bool }?,
    attribute x:scale  { Num }?
  }

# ---------------- Parametric shapes ----------------
QuickShape =
  element x:quickshape {
    attribute x:sides                 { xsd:positiveInteger },
    attribute x:circular              { Bool },
    attribute x:stellated             { Bool },
    attribute x:primary-curvature     { Bool },
    attribute x:stellation-curvature  { Bool },
    attribute x:stell-radius-ratio    { Num }?,
    attribute x:primary-curve-ratio   { Num }?,
    attribute x:stell-curve-ratio     { Num }?,
    attribute x:stell-offset-ratio    { Num }?,
    attribute x:centre                { Point },
    attribute x:major-axis            { Point },
    attribute x:minor-axis            { Point },
    attribute x:matrix                { Matrix6 }?,
    attribute x:reformed              { Bool }?,
    element x:edge-path { attribute d { text } }*,
    AnyForeign*
  }

# ---------------- Fills and transparencies ----------------
FillKind = "flat" | "linear" | "linear3" | "circular" | "elliptical"
         | "conical" | "diamond" | "three-point" | "four-point"
         | "bitmap" | "contone" | "fractal-clouds" | "fractal-plasma" | "noise"

Fill =
  element x:fill {
    attribute x:type       { FillKind },
    attribute x:repeat     { "simple" | "repeat" | "reflect" | "repeat-extra" }?,
    attribute x:profile    { Profile }?,
    attribute x:effect     { "fade" | "rainbow" | "alt-rainbow" }?,
    attribute x:stops      { text }?,          # "0:#rrggbb 0.5:#… 1:#…"
    attribute x:centre     { Point }?,
    attribute x:seed       { xsd:integer }?,
    attribute x:graininess { Num }?,
    attribute x:octaves    { xsd:positiveInteger }?,
    attribute x:dpi        { Num }?,
    element x:point { attribute x { Num }, attribute y { Num },
                      attribute x:colour { Colour } }*,
    AnyForeignAttr*, AnyForeign*
  }

BlendMode = "mix" | "stained-glass" | "bleach" | "darken" | "lighten"
          | "saturation" | "luminosity" | "hue" | "contrast" | "brightness"
          | "bevel" | "additive" | "subtractive" | "lut"

Transparency =
  element x:transparency {
    attribute x:type   { FillKind },
    attribute x:blend  { BlendMode }?,
    attribute x:amount { Num }?,
    attribute x:profile{ Profile }?,
    attribute x:stops  { text }?,
    AnyForeignAttr*, AnyForeign*
  }

# ---------------- Stroke ----------------
Stroke =
  element x:stroke {
    attribute x:type       { "plain" | "variable" | "brush" | "airbrush" },
    attribute x:base-width { Num }?,
    attribute x:profile    { Profile }?,
    element x:width-table  { text }?,          # "t:w t:w …"
    element x:pressure     { attribute x:encoding { "text" | "base64-u16le" }, text }?,
    element x:source-path  { attribute d { text } }?,
    AnyForeign*
  }

# ---------------- Live effects ----------------
Shadow =
  element x:shadow {
    attribute x:type     { "wall" | "floor" | "glow" | "inner" },
    attribute x:blur     { Num }, attribute x:offset { Point }?,
    attribute x:angle    { Num }?, attribute x:darkness { Num },
    attribute x:scale    { Num }?, attribute x:colour { Colour }?,
    attribute x:penumbra { Num }?
  }

Bevel =
  element x:bevel {
    attribute x:type            { "round" | "flat" | "chisel" | "ridge" | "mesa" },
    attribute x:indent          { Num },
    attribute x:light-angle     { Num },
    attribute x:light-elevation { Num }?,
    attribute x:contrast        { Num },
    attribute x:direction       { "inner" | "outer" },
    attribute x:join            { "round" | "miter" | "bevel" }?,
    attribute x:light-colour    { Colour }?,
    attribute x:shadow-colour   { Colour }?
  }

Contour =
  element x:contour {
    attribute x:width          { Num },
    attribute x:steps          { xsd:positiveInteger },
    attribute x:direction      { "inner" | "outer" | "both" },
    attribute x:join           { "round" | "miter" | "bevel" }?,
    attribute x:profile        { Profile }?,
    attribute x:colour-profile { Profile }?,
    attribute x:insets         { Bool }?
  }

Blend =
  element x:blend {
    attribute x:steps             { xsd:positiveInteger },
    attribute x:from              { IdRef },
    attribute x:to                { IdRef },
    attribute x:position-profile  { Profile }?,
    attribute x:attribute-profile { Profile }?,
    attribute x:one-to-one        { Bool }?,
    attribute x:antialias         { Bool }?,
    attribute x:path              { IdRef }?,
    attribute x:rotate-along-path { Bool }?,
    attribute x:start-angle       { Num }?,
    attribute x:end-angle         { Num }?,
    attribute x:tangential        { Bool }?,
    element x:blend-map { attribute x:pairs { text } }?,
    AnyForeign*
  }

Mould =
  element x:mould {
    attribute x:type   { "envelope" | "perspective" },
    attribute x:bounds { xsd:string },
    element x:mould-shape  { attribute d { text } },
    element x:mould-source { AnySvg* },
    AnyForeign*
  }

Feather = element x:feather { attribute x:width { Num },
                              attribute x:profile { Profile }? }

Effects =
  element x:effects {
    element x:effect {
      attribute x:id     { xsd:ID },
      attribute x:kind   { text },
      attribute x:locked { Bool }?,
      AnyForeignAttr*, AnyForeign*
    }+
  }

# ---------------- Colour ----------------
Palette =
  element x:palette {
    attribute x:id { text },
    element x:colour {
      attribute x:id     { xsd:ID },
      attribute x:name   { text }?,
      attribute x:model  { "rgb"|"cmyk"|"hsv"|"grey"|"spot"|"tint"|"shade"|"linked" },
      attribute x:srgb   { Colour },
      attribute x:cmyk   { text }?,
      attribute x:hsv    { text }?,
      attribute x:grey   { Num }?,
      attribute x:spot-name  { text }?,
      attribute x:parent     { IdRef }?,
      attribute x:amount     { Num }?,
      attribute x:hsv-delta  { text }?,
      attribute x:profile    { text }?,
      attribute x:screen-angle { Num }?,
      attribute x:solid-ink  { Bool }?,
      AnyForeignAttr*
    }+,
    AnyForeign*
  }

# ---------------- Loose attributes allowed on SVG elements ----------------
CommonXarastAttrs =
  attribute x:kind                { text }?,
  attribute x:generated           { text }?,
  attribute x:generated-by        { text }?,
  attribute x:generated-rev       { xsd:nonNegativeInteger }?,
  attribute x:generated-hash      { text }?,
  attribute x:base-authoritative  { Bool }?,
  attribute x:foreign-dirty       { Bool }?,
  attribute x:foreign-stale       { Bool }?,
  attribute x:names               { text }?,
  attribute x:fill-ref            { IdRef }?,
  attribute x:stroke-ref          { IdRef }?,
  attribute x:clone-of            { IdRef }?,
  attribute x:clone-mode          { "live" | "copy" }?,
  attribute x:locked              { Bool }?,
  attribute x:visible             { Bool }?,
  attribute x:printable           { Bool }?

AnySvg     = element svg:* { (attribute * { text } | AnySvg | text)* }
AnyForeignAttr = attribute * - x:* { text }
AnyForeign     = element   * - x:* { (attribute * { text } | AnyForeign | text)* }
```
### 11.3 `meta.rnc` — `meta.xml`

Omitted for length; its structure is that of §7.2, with the same open extensibility
rules (`AnyForeign*` at every level) and with `dc:*` taken from Dublin Core. The
complete schema lives in `crates/xarast-format/schemas/meta.rnc`.

### 11.4 Validation in CI

```bash
# Validation of the manifest and the metadata
trang schemas/manifest.rnc schemas/manifest.rng
xmllint --noout --relaxng schemas/manifest.rng  extracted/META-INF/manifest.xml
xmllint --noout --relaxng schemas/meta.rng      extracted/meta.xml

# Validation of the base SVG against the official SVG 1.1 schema
xmllint --noout --relaxng vendor/svg11.rng      extracted/document.svg

# Validation of the xarast: vocabulary (extracting only that subtree)
xarast-validate extracted/document.svg --schema schemas/xarast-doc.rng
```

CI **MUST** run all four steps over every file in the conformance corpus.

---

## 12. Complete example

A minimal `.xarast`: **a rectangle with a linear gradient (with a non-linear ramp
profile) over a background layer**, on two layers, A4 page.

> Everything that follows is **real and verified**: the files have been built, the XML
> has been checked to be well formed (`xmllint --noout`), the BLAKE3 digests have been
> computed with the reference implementation, and the ZIP listing and the hexadecimal
> dump are the literal output of `unzip -lv` and `od`. The sizes and CRCs add up.

### 12.1 ZIP listing

```console
$ unzip -lv ejemplo.xarast
Archive:  ejemplo.xarast
 Length   Method    Size  Cmpr    Date    Time   CRC-32   Name
--------  ------  ------- ---- ---------- ----- --------  ----
      26  Stored       26   0% 2026-09-19 17:41 8f97fcdc  mimetype
    1458  Defl:N      534  63% 2026-09-19 17:41 e509c5f1  META-INF/manifest.xml
    1841  Defl:N      771  58% 2026-09-19 17:41 7fed299a  meta.xml
    3375  Defl:N     1289  62% 2026-09-19 17:41 257a57e2  document.svg
     571  Stored      571   0% 2026-09-19 17:41 5cb1b71d  thumbnail.png
--------          -------  ---                            -------
    7271             3191  56%                            5 files
```

Total file size: **3,717 bytes**.

### 12.2 Signature verification (magic bytes)

```console
$ od -A d -t x1z -v ejemplo.xarast | head -4
0000000 50 4b 03 04 14 00 00 00 00 00 3b 8d 33 5d dc fc  >PK........;.3]..<
0000016 97 8f 1a 00 00 00 1a 00 00 00 08 00 00 00 6d 69  >..............mi<
0000032 6d 65 74 79 70 65 61 70 70 6c 69 63 61 74 69 6f  >metypeapplicatio<
0000048 6e 2f 76 6e 64 2e 78 61 72 61 73 74 2b 7a 69 70  >n/vnd.xarast+zip<
```

Breakdown of the local header of the first entry:

| Offset | Bytes | Field | Value |
|---|---|---|---|
| 0 | `50 4b 03 04` | signature | `PK\x03\x04` |
| 4 | `14 00` | version needed | 2.0 |
| 6 | `00 00` | flags | 0 (no encryption, no descriptor) |
| 8 | `00 00` | method | **0 = STORED** |
| 18 | `1a 00 00 00` | compressed size | 26 |
| 22 | `1a 00 00 00` | uncompressed size | 26 |
| 26 | `08 00` | name length | 8 |
| **28** | `00 00` | **extra field length** | **0** ← requirement of §3.2.1 |
| **30** | `6d 69 6d 65 74 79 70 65` | name | **`mimetype`** |
| **38** | `61 70 70 …` | content | **`application/vnd.xarast+zip`** |

### 12.3 `mimetype` (26 bytes, STORED, no trailing newline)

```
application/vnd.xarast+zip
```

### 12.4 `META-INF/manifest.xml`

```xml
<?xml version="1.0" encoding="UTF-8"?>
<mf:manifest xmlns:mf="https://xarast.org/ns/manifest/1.0"
             mf:version="1.0" mf:min-reader="1.0" mf:profile="portable"
             mf:generator="Xarast/0.1.0 (linux; x86_64)">
  <mf:file-entry mf:full-path="/" mf:media-type="application/vnd.xarast+zip"/>
  <mf:file-entry mf:full-path="mimetype" mf:media-type="text/plain"
                 mf:role="mimetype" mf:size="26" mf:method="stored"/>
  <mf:file-entry mf:full-path="META-INF/manifest.xml" mf:media-type="application/xml"
                 mf:role="manifest" mf:method="deflate"/>
  <mf:file-entry mf:full-path="meta.xml" mf:media-type="application/xml"
                 mf:role="meta" mf:size="1841" mf:method="deflate"
                 mf:digest="blake3-256"
                 mf:digest-value="0292ad76bf30d13dce31f284842a8b01f772b36a7974775ae9f2c0eb46a5f956"/>
  <mf:file-entry mf:full-path="document.svg" mf:media-type="image/svg+xml"
                 mf:role="document" mf:size="3375" mf:method="deflate"
                 mf:digest="blake3-256"
                 mf:digest-value="e07d64cd960dc94e50d9030c9d1c3333d8b91c877bc113e535ade8d3ba5adc47"/>
  <mf:file-entry mf:full-path="thumbnail.png" mf:media-type="image/png"
                 mf:role="thumbnail" mf:size="571" mf:method="stored"
                 mf:digest="blake3-256"
                 mf:digest-value="4ab11d84d1b953e36c278f709e317dd045c2242fdddcda28e5f1b35d02910607"/>
</mf:manifest>
```

Note that the manifest's own entry carries **no** digest (it cannot contain its own
hash) and no `mf:size` (the ZIP supplies it).

### 12.5 `meta.xml`

```xml
<?xml version="1.0" encoding="UTF-8"?>
<xarast:meta xmlns:xarast="https://xarast.org/ns/document/1.0"
             xmlns:dc="http://purl.org/dc/elements/1.1/"
             xarast:version="1.0" xarast:min-reader="1.0">
  <xarast:identity>
    <xarast:doc-id>01J9Q7ZB2K4M8N6P3R5T7V9W1X</xarast:doc-id>
    <xarast:revision>3</xarast:revision>
  </xarast:identity>
  <dc:title>Ejemplo mínimo</dc:title>
  <dc:creator>Ada Lovelace</dc:creator>
  <dc:language>es-ES</dc:language>
  <xarast:dates>
    <xarast:created>2026-09-19T17:02:11Z</xarast:created>
    <xarast:modified>2026-09-19T17:41:55Z</xarast:modified>
    <xarast:editing-duration>PT39M44S</xarast:editing-duration>
    <xarast:editing-cycles>3</xarast:editing-cycles>
  </xarast:dates>
  <xarast:generator xarast:name="Xarast" xarast:version="0.1.0" xarast:platform="linux-x86_64"/>
  <xarast:statistics xarast:spreads="1" xarast:pages="1" xarast:layers="2"
                     xarast:objects="2" xarast:bitmaps="0" xarast:fonts="0" xarast:colours="3"/>
  <xarast:units xarast:default="mm" xarast:precision="2"/>
  <xarast:page-setup xarast:width="210mm" xarast:height="297mm"
                     xarast:orientation="portrait" xarast:bleed="0mm" xarast:double-page="false"/>
  <xarast:grid xarast:kind="rectangular" xarast:origin="0 0" xarast:spacing="10mm"
               xarast:subdivisions="10" xarast:visible="false" xarast:snap="true"/>
  <xarast:guides>
    <xarast:guide xarast:orientation="vertical" xarast:position="105mm"/>
  </xarast:guides>
  <xarast:view xarast:zoom="0.75" xarast:scroll="0 0" xarast:quality="antialiased"
               xarast:active-layer="xL2"/>
  <xarast:nudge xarast:distance="1mm"/>
  <xarast:colour-management xarast:working-rgb="sRGB IEC61966-2.1"
                            xarast:rendering-intent="relative-colorimetric"/>
</xarast:meta>
```

### 12.6 `document.svg` (literal content, 3,375 bytes)

```xml
<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg"
     xmlns:xlink="http://www.w3.org/1999/xlink"
     xmlns:xarast="https://xarast.org/ns/document/1.0"
     xmlns:inkscape="http://www.inkscape.org/namespaces/inkscape"
     xmlns:sodipodi="http://sodipodi.sourceforge.net/DTD/sodipodi-0.dtd"
     xmlns:dc="http://purl.org/dc/elements/1.1/"
     xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     width="210mm" height="297mm" viewBox="0 0 595.276 841.89"
     version="1.1" id="xdoc">
<title>Ejemplo mínimo</title>
<desc>Un rectángulo con degradado, en dos capas.</desc>
<metadata><rdf:RDF><rdf:Description>
<dc:title>Ejemplo mínimo</dc:title><dc:creator>Ada Lovelace</dc:creator>
<dc:date>2026-09-19T17:41:55Z</dc:date></rdf:Description></rdf:RDF></metadata>
<defs>
<xarast:document xarast:version="1.0" xarast:min-reader="1.0" xarast:y-axis="down"
                 xarast:layout="single" xarast:colour-refs="literal"
                 xarast:duplicate-offset="10 10"
                 xarast:foreign-digest="blake3:0000000000000000" xarast:foreign-count="0"/>
<xarast:units xarast:default="mm" xarast:precision="2"/>
<xarast:palette xarast:id="doc">
  <xarast:colour xarast:id="c-azul"   xarast:name="Azul"   xarast:model="rgb" xarast:srgb="#1c3f8f"/>
  <xarast:colour xarast:id="c-ambar"  xarast:name="Ámbar"  xarast:model="rgb" xarast:srgb="#ffd166"/>
  <xarast:colour xarast:id="c-grafito" xarast:name="Grafito" xarast:model="rgb" xarast:srgb="#2b2b2b"/>
</xarast:palette>
<linearGradient id="g7a3" gradientUnits="userSpaceOnUse"
                x1="141.732" y1="255.118" x2="453.543" y2="255.118"
                spreadMethod="pad" xarast:profile="0.35 0"
                xarast:stops="0:#1c3f8f 1:#ffd166">
  <stop offset="0" stop-color="#1c3f8f"/>
  <stop offset=".125" stop-color="#2a4f9c"/>
  <stop offset=".25" stop-color="#3b60a8"/>
  <stop offset=".375" stop-color="#5173b3"/>
  <stop offset=".5" stop-color="#6c88bd"/>
  <stop offset=".625" stop-color="#8d9fc4"/>
  <stop offset=".75" stop-color="#b3b8c6"/>
  <stop offset=".875" stop-color="#dcc8bf"/>
  <stop offset="1" stop-color="#ffd166"/>
</linearGradient>
</defs>
<sodipodi:namedview id="base" units="mm" inkscape:document-units="mm"
                    showgrid="false" inkscape:current-layer="xL2">
  <sodipodi:guide position="297.638,0" orientation="1,0"/>
</sodipodi:namedview>
<g id="xSPREAD1" xarast:kind="spread" xarast:spread="1">
<xarast:page xarast:index="1" xarast:rect="0 0 595.276 841.89" xarast:bleed="0"/>
<g id="xL1" inkscape:groupmode="layer" inkscape:label="Fondo"
   xarast:kind="layer" xarast:layer-kind="normal" xarast:visible="true"
   xarast:locked="true" xarast:printable="true" xarast:solid="false"
   sodipodi:insensitive="true" style="display:inline">
<rect id="xk3m9q2vr7t" x="0" y="0" width="595.276" height="841.89" fill="#f4f1ea"/>
</g>
<g id="xL2" inkscape:groupmode="layer" inkscape:label="Ilustración"
   xarast:kind="layer" xarast:layer-kind="normal" xarast:visible="true"
   xarast:locked="false" xarast:printable="true" xarast:solid="false"
   xarast:active="true" style="display:inline">
<rect id="xp8w4n1zc6h" x="141.732" y="141.732" width="311.811" height="226.772"
      rx="11.339" fill="url(#g7a3)" stroke="#2b2b2b" stroke-width="2.835"
      xarast:fill-ref="#c-azul" xarast:stroke-ref="#c-grafito"/>
</g>
</g>
</svg>
```

### 12.7 Commentary on the example

The points it illustrates, one by one:

1. **The SVG is valid and self-contained.** No `xarast:` element affects the render: a
   browser paints the cream background, the rounded rectangle with the gradient and its
   graphite border, at the correct A4 physical size.
2. **The nine gradient stops are baked** from the profile `xarast:profile="0.35 0"`.
   Xarast, on opening, **discards** those nine and regenerates them from the profile and
   `xarast:stops="0:#1c3f8f 1:#ffd166"` (§6.4). An external viewer sees the curve
   approximated with an error ≤ 2/255.
3. **Both layers carry dual marking**: `inkscape:groupmode`/`inkscape:label` for
   Inkscape and `xarast:*` for Xarast. The locking of the background layer is expressed
   with `xarast:locked="true"` **and** with `sodipodi:insensitive="true"`, so that
   Inkscape respects it too.
4. **`<xarast:page>` is not rendered** because it is in a foreign namespace: the page
   geometry does not pollute the drawing.
5. **References to the palette** with `xarast:fill-ref`/`xarast:stroke-ref` alongside
   the literal value. The correct colour is seen anywhere and Xarast keeps the link.
6. **Coordinates in points with 3 decimals**: `595.276` pt = exactly 210 mm,
   `141.732` pt = 50 mm, `2.835` pt = 1 mm. Millipoint precision with no ambiguity.
7. **`xarast:foreign-count="0"`**: there is no unknown baggage. A writer **MAY** omit
   both attributes when the counter is zero; here they are emitted to illustrate them.
8. **Compression ratio 56 %** with only five entries and 7 KiB of content: on real
   documents the SVG's ratio rises to 6:1-12:1 (§4.6).

### 12.8 Reproducing the verification

```console
$ unzip -o ejemplo.xarast -d /tmp/ej && xdg-open /tmp/ej/document.svg   # render in a browser
$ xmllint --noout /tmp/ej/document.svg /tmp/ej/meta.xml /tmp/ej/META-INF/manifest.xml
$ b3sum /tmp/ej/document.svg
e07d64cd960dc94e50d9030c9d1c3333d8b91c877bc113e535ade8d3ba5adc47  /tmp/ej/document.svg
```

---


## 13. Rust implementation plan

### 13.1 Location in the crate tree

```
crates/
  xarast-model/      # document model (nodes, attributes, layers) — no I/O
  xarast-format/     # THIS format: ZIP container, manifest, meta
    schemas/         # *.rnc and *.rng
  xarast-svg/        # SVG profile: model <-> SVG serialisation/deserialisation
  xarast-render/     # rendering (for thumbnails, previews and baking)
  xarast-xar/        # importer for the binary .xar
  xarast-cli/        # `xarast` binary (extract, cat, repair, convert, validate)
```

`xarast-format` does **not** depend on `xarast-render`: thumbnail generation and baking
are injected through *traits* (`ThumbnailProvider`, `BakeProvider`) so that the format
crate remains testable without a graphics engine and without heavy dependencies.

### 13.2 Third-party crates

| Crate | Use | Notes |
|---|---|---|
| `zip` | Reading and writing the container | Supports `Stored`, `Deflated` and `Zstd` (method 93). Disable unnecessary features: `default-features = false, features = ["deflate", "zstd", "time"]` |
| `flate2` (`zlib-rs` or `miniz_oxide` backend) | Deflate | Pulled in by `zip`; pin the backend for reproducibility |
| `zstd` | `compact` profile | Only behind the `compact` feature |
| `blake3` | Digests and deduplication | Very fast; streaming hashing during import |
| `quick-xml` | SVG/XML parsing and serialisation | **Key**: one of the few that allow prefixes, comments and processing instructions to be preserved — a requirement of §8 |
| `memchr` | Scan acceleration in `quick-xml` | Transitive |
| `serde` + `serde_json` | Autosave `state.json`, NDJSON journal | Not for the manifest (that is XML) |
| `time` or `jiff` | RFC 3339 dates and ZIP DOS dates | |
| `ulid` or `uuid` (v7) | Persistent object IDs and `doc-id` | |
| `fs4` | Cross-platform `flock`/advisory locking | For §10.4 |
| `tempfile` | Atomic writing | `NamedTempFile::persist` |
| `thiserror` | Typed errors for the public API | |
| `tracing` | Diagnostics | |
| `resvg` + `usvg` + `tiny-skia` | **Only in `xarast-render`**: external validation and reference thumbnails in tests | |
| `image` | PNG thumbnail encoding | |
| `insta` | Snapshot tests of the serialised SVG | |
| `proptest` | Property-based round-trip | |
| `cargo-fuzz` + `arbitrary` | Fuzzing the reader | Mandatory (principle 5 of the vision) |
| `criterion` | Save/open benchmarks | |

Explicitly rejected: any binding to `libxml2` (C surface, XXE by default), and
`roxmltree` as the main parser (it is read-only and discards information needed for the
round-trip; it is useful in tests, though).

### 13.3 Proposed public API

```rust
// ============================ crates/xarast-format/src/lib.rs ============================

/// Format version that this code writes.
pub const FORMAT_VERSION: Version = Version { major: 1, minor: 0 };
pub const MIME_TYPE: &str = "application/vnd.xarast+zip";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version { pub major: u16, pub minor: u16 }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile { Portable, Compact }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method { Stored, Deflate, Zstd }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ResourceId(pub [u8; 32]);          // BLAKE3-256

// --------------------------------- READING ---------------------------------

pub struct XarastReader<R: Read + Seek> { /* … */ }

impl<R: Read + Seek> XarastReader<R> {
    /// Opens the container: validates the signature, reads the manifest and `meta.xml`.
    /// Does NOT parse `document.svg` (requirement O6: cheap opening).
    pub fn open(reader: R) -> Result<Self, ReadError>;

    /// Checks only the first 64 bytes. Useful for type detection.
    pub fn sniff(reader: &mut R) -> Result<bool, ReadError>;

    pub fn format_version(&self) -> Version;
    pub fn min_reader(&self) -> Version;
    pub fn profile(&self) -> Profile;
    pub fn manifest(&self) -> &Manifest;
    pub fn meta(&self) -> &DocumentMeta;

    /// Thumbnail without touching the document.
    pub fn thumbnail(&mut self) -> Result<Option<Vec<u8>>, ReadError>;
    pub fn preview(&mut self, spread: u32) -> Result<Option<Vec<u8>>, ReadError>;

    /// Raw bytes of an entry; verifies the manifest digest.
    pub fn entry(&mut self, path: &str) -> Result<Vec<u8>, ReadError>;
    pub fn entry_stream(&mut self, path: &str) -> Result<impl Read + '_, ReadError>;
    pub fn resource(&mut self, id: ResourceId) -> Result<Vec<u8>, ReadError>;
    pub fn entries(&self) -> impl Iterator<Item = &FileEntry>;

    /// Parses the whole document into the model. This is where the cost is paid.
    pub fn document(&mut self, opts: &LoadOptions) -> Result<LoadedDocument, ReadError>;

    /// Consumes the reader and returns the preservation context (§8) so that
    /// the file can be rewritten without losing anything.
    pub fn into_preservation(self) -> PreservationContext;
}

#[derive(Debug, Default, Clone)]
pub struct LoadOptions {
    /// Regenerate the `xarast:generated` subtrees (default: yes).
    pub rebake: bool,
    /// Anti zip-bomb / malicious-XML limits (§10.5).
    pub limits: Limits,
    /// Reject rather than warn when a digest does not match.
    pub strict: bool,
}

pub struct LoadedDocument {
    pub document: xarast_model::Document,
    pub preservation: PreservationContext,
    pub diagnostics: Vec<Diagnostic>,     // warnings, not errors
}

// --------------------------------- WRITING ---------------------------------

pub struct XarastWriter<W: Write + Seek> { /* … */ }

#[derive(Debug, Clone)]
pub struct WriteOptions {
    pub profile: Profile,
    pub deflate_level: u8,        // 0..=9
    pub zstd_level: i32,          // -7..=22
    pub deterministic: bool,      // O8: no varying timestamps, fixed order
    pub fixed_mtime: Option<OffsetDateTime>,
    pub pretty: bool,             // indent the SVG (for git)
    pub thumbnail: bool,
    pub previews: bool,
    pub embed_fonts: bool,
    pub materialize_derived: bool, // false = regenerate derived renditions on open (§4.4)
    pub history: HistoryPolicy,
}

impl<W: Write + Seek> XarastWriter<W> {
    pub fn new(writer: W, opts: WriteOptions) -> Self;

    /// Re-injects the previously read unknown baggage (§8). If omitted,
    /// the result is NOT a lossless round-trip and `finish()` flags it.
    pub fn with_preservation(self, ctx: PreservationContext) -> Self;
    pub fn with_thumbnail_provider(self, p: Arc<dyn ThumbnailProvider>) -> Self;
    pub fn with_bake_provider(self, p: Arc<dyn BakeProvider>) -> Self;

    pub fn write_document(&mut self, doc: &xarast_model::Document) -> Result<(), WriteError>;
    pub fn add_resource(&mut self, kind: ResourceKind, bytes: &[u8]) -> Result<ResourceId, WriteError>;
    pub fn finish(self) -> Result<WriteReport, WriteError>;
}

pub struct WriteReport {
    pub bytes_written: u64,
    pub entries: usize,
    pub resources_deduplicated: usize,
    pub bytes_saved_by_dedup: u64,
    pub preservation_complete: bool,
    pub warnings: Vec<Diagnostic>,
}

/// Full atomic save (§10.1): temporary + fsync + rename + fsync of the directory.
pub fn save_atomic(
    path: &Path,
    doc: &xarast_model::Document,
    ctx: Option<&PreservationContext>,
    opts: &WriteOptions,
) -> Result<WriteReport, WriteError>;

// --------------------------------- PRESERVATION ---------------------------------

/// Everything the reader did not understand and the writer MUST put back in place.
#[derive(Debug, Default, Clone)]
pub struct PreservationContext {
    /// Foreign attributes and subtrees, indexed by the model's node id.
    pub node_baggage: HashMap<NodeId, ForeignBaggage>,
    /// Unknown ZIP entries, with their compressed stream intact.
    pub unknown_entries: Vec<RawZipEntry>,
    /// Unknown sections of `meta.xml`.
    pub meta_baggage: Vec<ForeignFragment>,
    /// Comments and processing instructions with their position.
    pub comments: Vec<PositionedNode>,
    pub foreign_digest: Option<[u8; 32]>,
    pub foreign_count: usize,
}

impl PreservationContext {
    /// Recomputes the canonical digest (§8.4).
    pub fn digest(&self) -> [u8; 32];
    /// Compares with the stored digest; `Err` if something has been lost.
    pub fn verify(&self, stored: &[u8; 32]) -> Result<(), PreservationLoss>;
}

// --------------------------------- LOCKING AND RECOVERY ---------------------------------

pub struct DocumentLock { /* … */ }
impl DocumentLock {
    pub fn acquire(doc_path: &Path) -> Result<Self, LockError>;   // §10.4
    pub fn holder(&self) -> Option<&LockHolder>;
    pub fn steal_if_stale(doc_path: &Path) -> Result<Self, LockError>;
}

pub struct AutosaveSession { /* … */ }
impl AutosaveSession {
    pub fn begin(doc_id: &DocId) -> Result<Self, IoError>;
    pub fn snapshot(&mut self, doc: &xarast_model::Document) -> Result<(), IoError>;
    pub fn journal(&mut self, op: &JournalEntry) -> Result<(), IoError>;
    pub fn discard(self) -> Result<(), IoError>;
    pub fn recoverable() -> Result<Vec<RecoveryCandidate>, IoError>;
}
```

In `xarast-svg`:

```rust
pub struct SvgProfile { /* serialisation options */ }

pub fn write_svg(
    doc: &Document, ctx: Option<&PreservationContext>,
    bake: &dyn BakeProvider, opts: &SvgProfile, out: &mut dyn Write,
) -> Result<SvgWriteStats, SvgError>;

pub fn read_svg(
    input: &[u8], limits: &Limits,
) -> Result<(Document, PreservationContext, Vec<Diagnostic>), SvgError>;

/// Baking: all the logic of §5.4 and §6 lives behind this trait.
pub trait BakeProvider: Send + Sync {
    fn bake_gradient_profile(&self, g: &GradientSpec) -> Vec<GradientStop>;
    fn bake_effect(&self, e: &LiveEffect, target: &Node) -> BakedSubtree;
    fn bake_fill(&self, f: &Fill, bounds: Rect) -> BakedFill;   // may rasterise
    fn bake_stroke(&self, s: &Stroke, path: &Path) -> BakedSubtree;
}
```

### 13.4 Implementation details that must be got right

1. **`quick-xml` in preserving mode.** Use `Reader` with `check_end_names(true)`,
   `trim_text(false)` and **do not** expand entities. When writing, use `Writer` with
   the same quotes and escapes as the original for the preserved fragments: baggage
   fragments are re-emitted as **raw bytes** (`Event::Text` with
   `BytesText::from_escaped`) to guarantee byte-for-byte identity.
2. **`mimetype` entry with no extra field.** The `zip` crate may add extra fields
   (extended timestamps, alignment). That entry must be built with
   `SimpleFileOptions::default().compression_method(Stored).last_modified_time(fixed)`
   and it must be **verified in a test** that byte 28 is `00 00` and that bytes 38..64
   are the MIME type. It is a one-line test that protects type detection for the entire
   format.
3. **Copying compressed streams without recompressing.** For preserved entries and for
   resources that have not changed, use `ZipWriter::raw_copy_file` (avoids
   decompressing and recompressing megabytes on every save). It is the difference
   between saving a photographic document in 0.2 s and in 8 s.
4. **Streaming hashing.** `blake3::Hasher` over the reader, without materialising the
   whole resource in memory when it exceeds a few MiB.
5. **Deterministic writing.** `deterministic: true` ⇒ fixed DOS date
   (`1980-01-01 00:00:00`, the minimum representable), fixed external attributes
   (`0o644`), no extra fields, canonical entry and attribute order. Test: saving the
   same model twice produces byte-identical files.
6. **Limits before allocating.** All the limits of §10.5 are checked **before**
   reserving memory, not after.
7. **No `unsafe`.** `#![forbid(unsafe_code)]` in `xarast-format` and `xarast-svg`.

### 13.5 Tests

#### a) Structural round-trip (mandatory)

```rust
#[test] fn roundtrip_modelo_identico() {
    for caso in corpus() {
        let doc0 = load(&caso);
        let bytes = save_to_vec(&doc0);
        let doc1 = load_from_bytes(&bytes);
        assert_eq!(doc0.canonical(), doc1.canonical());   // structural equality
    }
}
```

#### b) Byte-stable round-trip (O8)

```rust
#[test] fn guardar_dos_veces_es_identico() {
    let doc = load(caso);
    let a = save_to_vec_deterministic(&doc);
    let b = save_to_vec_deterministic(&load_from_bytes(&a));
    assert_eq!(a, b);            // fixed point on the first re-save
}
```

#### c) Preservation of the unknown (§8.7) — **the most important test**

```rust
#[test] fn preserva_el_100_por_cien_del_equipaje_ajeno() {
    let inyectado = inject_foreign_ns(load_raw(caso), "urn:test:future");
    let doc = load_from_bytes(&inyectado);
    let doc = aplicar_ediciones(doc);        // move, recolour, group, undo
    let salida = save_to_vec(&doc);
    assert_foreign_intact(&inyectado, &salida, "urn:test:future");
}
```

#### d) Signature and detection

```rust
#[test] fn magic_bytes_en_offsets_fijos() {
    let z = save_to_vec(&Document::empty());
    assert_eq!(&z[0..4],   b"PK\x03\x04");
    assert_eq!(&z[28..30], &[0, 0]);                 // no extra field
    assert_eq!(&z[30..38], b"mimetype");
    assert_eq!(&z[38..64], MIME_TYPE.as_bytes());
}
```

#### e) Render conformance (§5.6)

Golden tests: for each file in the corpus, render with Xarast and with `resvg`,
Chromium and (if installed) Inkscape, and check the SSIM thresholds of the table in
§5.6. The reference PNGs are stored in Git LFS.

#### f) Schema validation

Run `xmllint --relaxng` over the manifest, `meta.xml` and SVG of every generated file
(§11.4).

#### g) Deduplication

```rust
#[test] fn ocho_usos_una_entrada() {
    let doc = documento_con_la_misma_imagen_n_veces(8);
    let rep = save_report(&doc);
    assert_eq!(rep.entries_under("resources/images/"), 1);
    assert!(rep.bytes_saved_by_dedup > 7 * IMG_LEN - 4096);
}
```

#### h) Compression

```rust
#[test] fn los_jpeg_no_se_recomprimen() {
    let rep = save_report(&documento_con_jpeg());
    assert_eq!(rep.method_of("resources/images/b3-*.jpg"), Method::Stored);
}
```

#### i) Fuzzing (mandatory from day 1)

```
fuzz/fuzz_targets/
  fuzz_open.rs       # XarastReader::open over arbitrary bytes
  fuzz_document.rs   # parsing of document.svg over arbitrary XML
  fuzz_roundtrip.rs  # load → save → load, checking for no panic and no divergence
```
Acceptance criterion: 24 h of `cargo fuzz` with no findings before each release.

#### j) Properties (`proptest`)

Generate random documents from the model and check:
`load(save(d)) == d`, `save(load(save(d))) == save(d)`, and that no combination of
write options changes the recovered model.

#### k) Interoperability with Inkscape (manual, at every release)

Open in Inkscape, move an object, save, reopen in Xarast: the layers, the guides and
**all** the `xarast:` extensions must still be there (it is the real check of §8.4).

### 13.6 Suggested implementation order

| Step | Deliverable | Project phase |
|---|---|---|
| 1 | Container: read/write ZIP, `mimetype`, manifest, `meta.xml`, tests (d) and (b) | v0.1 |
| 2 | **Core** SVG profile: paths, shapes, groups, layers, flat fills and linear/radial gradients, stroke, bitmaps; tests (a), (f) | v0.1 |
| 3 | Preservation and round-trip of the unknown; tests (c) | v0.1 (**do not postpone**: it is far more expensive to add later) |
| 4 | Deduplication, compression policy, atomic writing, locking, autosave; tests (g), (h) | v0.1 |
| 5 | Text, clips, masks, palette, CMYK | v0.2 |
| 6 | Live effects and baking (full `BakeProvider`), exotic fills | v0.3 |
| 7 | `history/`, `compact` profile (zstd), split layout | v1.0 |

> **Critical design note:** step 3 **MUST** be in v0.1, even though there is nothing to
> preserve yet. If the document model is not born with the baggage container on every
> node, adding it later means touching the whole model, the whole parser and the whole
> serialiser. It is the direct lesson of §2.4.

---

## 14. Appendices

### 14.1 Summary of decisions and rejected alternatives

| Decision | Rejected alternative | Reason |
|---|---|---|
| ZIP container | Tar+zstd, SQLite, a bespoke format | Interoperability, ubiquitous tooling, random access |
| `mimetype` STORED first | File extension only | Magic-based detection without decompressing |
| XML manifest | `manifest.json` | Homogeneity of the XML chain, validation with `xmllint`, namespaces (§3.5) |
| One `document.svg` | One SVG per spread; `content.xml`+`styles.xml` | Direct opening in a browser; no cross-references |
| Namespace-based extensions | `data-*` attributes; an attached binary blob | Standard, ignorable by construction, readable |
| Dual representation with baking | Parametric only; baked only | Satisfies O1 and O2 simultaneously |
| Unit = PostScript point | CSS pixel; millipoint directly | Exact millipoint precision with 3 decimals |
| Y axis downwards | Global `scale(1,-1)` | Does not break text, gradients or filters |
| BLAKE3-256 | SHA-256, xxHash, CRC | Fast and cryptographically sound |
| Deflate by default, zstd optional | zstd always | Read interoperability with common tools |
| Binaries STORED | Recompress everything | CPU cost with no saving |
| Journal outside the package | Journal inside the ZIP | Rewriting the ZIP per operation is unworkable |

### 14.2 Conformance levels

| Level | Name | A **reader** at this level… | A **writer** at this level… |
|---|---|---|---|
| **A** | Core | Opens the container, reads the manifest and metadata, and renders the base SVG. It does not understand `xarast:` but **MUST** preserve it (§8) | Produces a valid container and base SVG, with minimal `xarast:` (document, pages, layers) |
| **B** | Complete | Understands the whole v1.0 `xarast:` vocabulary, regenerates the baking, edits with full fidelity | Emits the complete vocabulary and the baking of §5.4 |
| **C** | Archival | Level B + verifies all digests, requires embedded fonts and text duplicated as curves, rejects references to missing resources | Level B + embeds fonts and ICC profiles, materialises all derived renditions, emits an empty `history/` and a `README.txt` |

Xarast v0.1 targets level **B** for the v0.1 subset and level **A** in full.
Third-party tools (importers, viewers, indexers) have in level **A** a target
achievable in a few hours of work.

### 14.3 Registry of effect `xarast:kind` values (v1.0)

Effects with a defined baking to an SVG filter. Extending this table does **NOT** raise
`min-reader` (§7.4, rule 4).

| `xarast:kind` | Parameters | Baking |
|---|---|---|
| `gaussian-blur` | `radius` | `feGaussianBlur` |
| `sharpen` / `unsharp` | `radius`, `amount` | `feConvolveMatrix` or `feGaussianBlur`+`feComposite arithmetic` |
| `levels` | `black`, `white`, `gamma` | `feComponentTransfer type="gamma"` + `linear` |
| `brightness-contrast` | `brightness`, `contrast` | `feComponentTransfer type="linear"` |
| `hue-saturation` | `hue`, `saturation`, `lightness` | `feColorMatrix type="hueRotate"` + `saturate` |
| `greyscale` | `method` | `feColorMatrix type="saturate" values="0"` |
| `sepia` | `amount` | `feColorMatrix` |
| `invert` | — | `feComponentTransfer type="table" tableValues="1 0"` |
| `posterize` | `levels` | `feComponentTransfer type="discrete"` |
| `noise` | `amount`, `seed` | `feTurbulence` + `feComposite` |
| `emboss` | `angle`, `depth` | `feConvolveMatrix` |
| `displace` | `map`, `scale` | `feDisplacementMap` |

### 14.4 Conformance checklist for a `.xarast`

- [ ] First entry = `mimetype`, STORED, no extra field, exactly 26 bytes
- [ ] Bytes 0..4 = `PK\x03\x04`; 30..38 = `mimetype`; 38..64 = the MIME type
- [ ] `META-INF/manifest.xml` present, validates against `manifest.rnc`
- [ ] Manifest entry `/` with `media-type` = contents of `mimetype`
- [ ] One manifest entry per ZIP entry; no duplicates
- [ ] Correct BLAKE3 digests for `document`, `meta` and every `resource`
- [ ] `document.svg` well formed, validates against SVG 1.1, no `<script>`, no DTD
- [ ] All resource references are relative and resolve inside the package
- [ ] `id`s unique throughout the document
- [ ] Every `xarast:generated` subtree has an `xarast:generated-by` that resolves
- [ ] No entry name with `..`, `\`, a leading `/` or control characters
- [ ] Decompression ratio of each entry < 200:1
- [ ] Mean corpus SSIM ≥ 0.90 against the native render

### 14.5 Future work (v1.1+)

1. **Deltas in `history/`** (`*.vcdiff`) instead of full snapshots.
2. **Digital signatures** (`META-INF/signatures.xml`, XMLDSig over the manifest
   digests) and **encryption** (`META-INF/encryption.xml`, AES-GCM per entry).
3. **Flat model with `nodeChanges`** for incremental synchronisation and collaboration,
   taking advantage of the stable IDs of §5.7 (the Figma lesson, §2.5).
4. **IANA registration** of the type `application/vnd.xarast+zip`.
5. **`web` profile**: a writer variant producing an SVG optimised for serving directly
   (no extensions, with a consolidated `<style>` and a cropped `viewBox`).
6. **Real colour meshes** if `svg-next` stabilises `<meshgradient>`; until then, baking
   (§6.3).

### 14.6 References

- OASIS, *Open Document Format for Office Applications v1.2/1.3, Part 3: Packages* —
  https://docs.oasis-open.org/office/v1.2/cs01/OpenDocument-v1.2-cs01-part3.html
- Krita, *file_kra* — https://docs.krita.org/en/general_concepts/file_formats/file_kra.html
  and https://github.com/2shady4u/godot-kra-psd-importer/blob/master/docs/KRA_FORMAT.md
- Scribus, `.sla` structure — http://justsolve.archiveteam.org/wiki/Scribus
- Inkscape, *Inkscape-specific XML attributes* —
  https://wiki.inkscape.org/wiki/Inkscape-specific_XML_attributes
  and *Inkscape SVG vs. plain SVG* — https://wiki.inkscape.org/wiki/Inkscape_SVG_vs._plain_SVG
- W3C, *SVG 1.1 (Second Edition), 23 Extensibility* —
  https://www.w3.org/TR/2011/REC-SVG11-20110816/extend.html
- Libre Arts, *Gradient meshes and hatching to be removed from SVG 2.0* —
  https://librearts.org/2018/05/gradient-meshes-and-hatching-to-be-removed-from-svg-2-0/
- Figma `.fig` / Kiwi — https://github.com/OpenFig-org/openfig-core/blob/main/docs/research.md
- Xara, *Xar Format Specification* — http://site.xara.com/support/docs/webformat/spec/XARFormatDocument.pdf
- freedesktop.org, *Shared MIME-info Database* —
  https://specifications.freedesktop.org/shared-mime-info-spec/latest-single/
- Wikipedia, *ZIP (file format)* — compression methods and the change from ID 20 to 93
  for Zstandard — https://en.wikipedia.org/wiki/ZIP_(file_format)
- crate `zip` — https://docs.rs/zip/latest/zip/enum.CompressionMethod.html
- crate `blake3` — https://docs.rs/blake3
- Original source code: `/home/user/xara-xtreme/Kernel/cxftags.h` (209 tags),
  `Kernel/fillval.h` (`FILLSHAPE_*`, `RepeatType`, `TranspType`),
  `Kernel/nodershp.h` (`NodeRegularShape`), `Kernel/doccoord.h` (millipoints),
  `Mime/xaralx.xml`, `xaralx.desktop`.
