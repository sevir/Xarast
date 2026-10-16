# export

Memory note for **export** (`xarast-io`, the export entry of
`xarast-render`, and `xarast-cli export`). Phase 11:
`docs/phases/phase-11-export-filters.md`.

## Current state

Round 1 (2026-09-23, XARA-US-0056 + XARA-US-0057): the export model, the
filter registry and deterministic **raster** export to PNG, JPEG and WebP.
Round 2 (2026-09-23, XARA-US-0059): **vector PDF 1.7** export, one page per
export, with the fidelity ladder (section "PDF" below). Round 3
(2026-09-24, XARA-US-0058): **SVG** export through the `.xarast` profile's
mapper in its `Interchange` dialect (section "SVG" below). Round 4
(2026-09-24, XARA-US-0060): **colour fidelity and the export regression
harness** (section "Colour fidelity and regression" below). Batch export
(T11.1.6), palette quantisation
(T11.2.8) and AVIF (T11.2.7) are not built yet. Round 6 (2026-10-16,
XARA-T-0192): **File › Export** — `impl ExportSource for Session` in
`xarast-app`, exports off the interface thread with cancel, and the
dialog generated from `Capabilities` (section "File › Export" below). Round 5 (2026-09-24, XARA-T-0228 + XARA-T-0233 + XARA-T-0245):
**fonts** — PDF text is live text in embedded subset fonts with
`ToUnicode`, SVG embeds WOFF2 subsets, `--text outlines` for both, and
faces whose `fsType` forbids embedding become outlines (section "Fonts"
below).

| Piece | Where |
|---|---|
| `ExportRequest`, `ExportArea`, `ExportSizing` (+ `SizingEdit`), `Background` | `crates/xarast-io/src/model.rs` |
| `FormatId`, `FormatOptions` and the per-format option structs (serde) | `crates/xarast-io/src/options.rs` |
| `Exporter`, `Capabilities`, `Registry`, `XAR_EXPORT_REFUSAL` | `crates/xarast-io/src/registry.rs` |
| `ExportSource` (the app seam), `SceneSource`, `Progress`, `CancelFlag` | `crates/xarast-io/src/source.rs` |
| `ExportReport`, `Compromise`, `ExportError` | `crates/xarast-io/src/report.rs` |
| The three raster exporters, atomic writes | `crates/xarast-io/src/raster.rs` |
| PNG writer (ours), parallel zlib, JPEG and WebP wrappers | `png.rs`, `deflate.rs`, `jpeg.rs`, `webp.rs` |
| The export rasteriser (strips on the CPU backend) | `crates/xarast-render/src/export.rs`, `CpuBackend::render_rows` |
| `impl ExportSource for Session`, `ExportJob`, `ExportWorker`, `ExportProgress` | `crates/xarast-app/src/export.rs` |
| The File › Export dialog's model (`ExportDialog`, `ExportDialogView`, `ExportDialogOp`) | `crates/xarast-app/src/export_dialog.rs` |
| The dialog's widgets (`dialogs::export`) | `crates/xarast-ui/src/dialogs.rs` |
| `xarast-cli export` (hands its `Session` to the registry) | `crates/xarast-cli/src/export.rs` |
| `PdfExporter`, `plan_page`, the command translator and the ladder | `crates/xarast-io/src/pdf/mod.rs` |
| `PdfWriter` façade (the only code naming `pdf-writer`) | `crates/xarast-io/src/pdf/writer.rs` |
| Gradients → shadings, graduated transparency → soft-mask content | `crates/xarast-io/src/pdf/shading.rs` |
| Text runs → embedded `CIDFont`s, `ToUnicode`, text operators | `crates/xarast-io/src/pdf/text.rs`, `PdfWriter::font`, `Canvas::{begin_text, glyph, end_text}` |
| `SceneText`, `TextRun`, `ExportGlyph`; `ExportSource::text_as_outlines` | `crates/xarast-io/src/source.rs` |
| `TextOutput` (`--text text\|outlines`) | `crates/xarast-io/src/options.rs` |
| Ladder step 3: object / backdrop rasters | `crates/xarast-io/src/pdf/rasterise.rs`, `DisplayList::with_commands`, `export::ListRasteriser` |
| `PdfOptions`, `PdfVersion`, `BlendFidelity` | `crates/xarast-io/src/options.rs` |
| `SvgExporter`, the bitmap linker, the report mapping | `crates/xarast-io/src/svg.rs` |
| `SvgOptions`, `SvgResources` (export side) | `crates/xarast-io/src/options.rs` |
| `SvgDialect`, `BitmapLinker`, `SvgOptions::{area, background, minify}` (mapper side) | `crates/xarast-format/src/svg/mod.rs` |
| The interchange projection | `crates/xarast-format/src/svg/interchange.rs` |
| `cargo xtask svg-check [--interchange]` (usvg + resvg) | `xtask/src/main.rs` |
| Colour census, `ColourConverted` / `ProfileDropped`, the sRGB marker table | `crates/xarast-io/src/fidelity.rs` |
| `png::with_icc_profile` (`sRGB` → `iCCP`) | `crates/xarast-io/src/png.rs` |
| `xarast-cli fixtures` (colour sheet, features, synthetic) | `crates/xarast-cli/src/fixtures.rs` |
| `cargo xtask export-check` (SVG/PDF vs PNG, `qpdf --check`) + limits files | `xtask/src/export_check.rs`, `xtask/export-limits-{fixtures,corpus}.txt` |
| CI: job `export` (two machines) + `reproducible`; nightly `export-corpus.yml` | `.github/workflows/` |

The whole 59-file corpus exports to all three formats with **zero errors**,
at 96 and at 300 dpi, and two runs are byte-identical for every file and
format (numbers in `perf.md`, "Export"). The only compromises the corpus
reports are 25 font substitutions and one text-on-path drawn straight
(plus `AlphaFlattened` for every JPEG over a transparent background).

## SVG (W11.3, XARA-US-0058)

### One mapper, two dialects

There is **one** SVG mapper, `xarast_format::svg::write_svg`. `.xarast`
calls it with `SvgDialect::Native` (the default); `SvgExporter` calls it
with `SvgDialect::Interchange`. Everything the phase lists as shared —
element mapping, gradients, ramp baking, masks with
`color-interpolation="sRGB"`, blend modes, clips, text placement — is the
same code. What Interchange changes, and where:

| Aspect | Native | Interchange | Where |
|---|---|---|---|
| `xarast:` attributes and elements | written | removed, an element with its subtree; `xmlns:xarast` too | `interchange::project`, the last step of `write_svg` |
| Foreign baggage (unknown data from a load) | re-emitted | not written; nodes counted in `Stats::foreign_omitted` → `Compromise::UnknownDataDropped` | `Emitter::common` / `fragment` |
| Foreign namespace declarations | written | not written | `assemble` |
| Bitmaps | `resources/…` in the `ResourceIndex` (+ palette blobs) | whatever `SvgOptions::bitmaps` (a `BitmapLinker`) returns | the href closure in `write_svg` |
| Photo-adjusted bitmap objects (W10.6) | the master + `<xarast:photo-ops>` | the adjusted pixels baked into a PNG by `SvgOptions::derived_bitmaps` (a `DerivedLinker`); the master when none | `Emitter::bitmap` |
| `href` + `xlink:href` on images | both | `xlink:href` only (the root says `version="1.1"`; stops a `data:` image being stored twice) | `interchange::write_attrs` |
| Root frame | first spread's pages | `SvgOptions::area` (the export area, bleed included), same coordinates | `write_svg` → `ViewBox` |
| Background | none | `SvgOptions::background` → a `<rect>` over the viewBox, first in the body | `assemble` |
| Ids | stable, the identity model | kept; `minify` drops those nothing refers to (`url(#…)`, `href="#…"`, `inkscape:current-layer`) plus comments and indentation | `interchange::project` |
| Baked content | `xarast:generated` | unmarked (the attribute goes with the rest) | projection |
| Shadows (XARA-T-0320, floor + feathered T-0324, T-0327) | `xarast:shadow` + `filter="url(#f…)"` on the controller to a `<filter xarast:filter="shadow">` (a feathered wall or glow: the same filter feathers both); floor: a derived `<g xarast:derived>` copy | the same, unmarked | `Emitter::bake_shadow`, `svg/effect.rs` |
| Feathers (XARA-T-0317) | `xarast:feather` + `filter="url(#f…)"` to a `<filter xarast:filter="feather">` | the same filter, unmarked | `Emitter::own_feather`, `svg/effect.rs` |

**Why a projection pass instead of an `if` at every write.** The
parametric layer is ~290 emission sites across `emit.rs`, `paint.rs`,
`style.rs` and `text.rs`, many of them whole elements pushed as strings.
Guarding each one would fork the mapper in all but name and rot the first
time someone adds a twin. The projection is ~200 lines, runs once over the
writer's own (well-formed, `"`-quoted, escaped) output, and makes "no
`xarast:` in interchange" true by construction. It is not a general XML
parser and must not be fed foreign text — which is why baggage is dropped
in the emitter, not in the projection. Anything that must *differ in
content* (not just disappear) between dialects goes into the mapper
behind `SvgOptions::dialect`; that list is the table above.

### How it works

- `ExportSource` gained two defaulted methods (additive):
  `document() -> Option<&Document>` and `svg_text_placer() -> Option<Placer>`.
  SVG export maps the model, not the scene, so a scene-only source
  (`SceneSource`) is refused with `UnsupportedFormat`; `build_scene` is
  never called. `Session` (and the detached copy an `ExportJob` exports)
  returns its document and `xarast_app::svg_text::placer()`.
- **Bitmaps** (`svg.rs`, `Links`): PNG / JPEG / GIF originals **with no
  reconstruction palette** pass through byte for byte; anything else (the
  `.xar` BMP flavours, JPEG8BPP, pixel-only bitmaps) is decoded by the
  walker's rules (tag 71 with palette, 65, 69) and written as an RGBA PNG
  by our encoder. Deduplicated by encoded bytes. `Inline`: `data:` URIs.
  `Sidecar`: `<stem>_files/image-N.ext`, N in first-use (document) order,
  the folder name sanitised to `[A-Za-z0-9._-]` (resvg does not
  percent-decode relative paths; with `%20` the images vanished, mean |Δ|
  1.5 → 12.6 on scope3). Each file is written atomically.
- **Photo adjustments** (XARA-US-0054): a bitmap object with a chain is
  written as a PNG of `xarast_io::photo::bake` over the master's pixels
  (read as `pixels` reads any bitmap; the master's ICC profile goes in
  `iCCP`), deduplicated with the other images by bytes. A JPEG master
  therefore becomes a PNG — the report says `Simplified { "photo
  adjustments baked into a PNG of the adjusted pixels", count }`; a chain
  that cannot be baked shows the master and is `NotRendered { "photo
  adjustments (the image is shown unadjusted)" }`. Raster exports and PDF
  need nothing: they render the scene, whose image is already the
  derived one (PDF rasterises placed images until T11.4.8 anyway, with
  its existing `Rasterised` compromise). Tested:
  `tests/svg.rs::photo_adjustments_are_baked_into_a_png_of_their_own`
  (the sidecar PNG decodes to exactly `bake`'s pixels).
- **Report**: the writer's counters become `NotRendered` (missing images,
  outline-less quick shapes, unsupported clips, unknown `.xar` records,
  arrowheads, feathering) and the new **`Compromise::Simplified { what,
  count }`** (approximated fills, perspective, contrast/brightness blends,
  variable-width strokes, text on a path left straight — since XARA-T-0252
  only reflected or sheared characters, or no text placer); the writer
  counts per kind, not
  per object, hence a new variant rather than `Approximated { node }`.
  Fonts are embedded (T11.3.4, "Fonts" below): `FontNotEmbedded` names
  only a face that was not (its licence refuses it, so its stories are
  outlines; or subsetting failed); without a text placer every family
  is still reported. `pixels` is the area in points, `dpi` 72,
  `commands` the elements written, `render_time` the mapping.
- CLI: `--format svg` / `-o x.svg`, `--resources inline|sidecar`,
  `--minify`, `--pretty`; `--background` works (paper/colour → the rect).

### Validation (2026-09-24, after XARA-T-0231 merged)

- `crates/xarast-io/tests/svg.rs` (5): no `xarast`, only SVG / metadata /
  Inkscape prefixes, viewBox = area, `UnknownDataDropped`, inline and
  sidecar images (one reference per image, deduplicated), byte-identical
  runs (plain, minify, pretty), minify + background, scene-only refusal,
  cancel leaves nothing. `interchange` unit tests: projection, idempotent
  minify, quoted `>`, single image reference. CLI: a small `.xar` to SVG,
  flag refusals, cross-process determinism, and the corpus test exports
  SVG too and greps for `xarast:`.
- Corpus (59 files, `--background paper`): 59/59 exported, 82.9 MB, two
  runs byte-identical; `cargo xtask svg-check --interchange` parses and
  renders all 59 inline and all 59 sidecar+minify files.
- **resvg vs our PNG export** (72 dpi, paper, SVG rendered at the PNG's
  width with `cargo xtask svg-render`): median mean |Δ| **1.30/255**,
  52/59 files under 4/255, 8 templates exact. Worst: Fill Types simple
  20.0 (conical/diamond/multi-colour fills — the profile's bake ladder,
  XARA-T-0102), WATCH2 17.0 (its background is such a fill), TextJust 14.9
  (glyph rendering of small text; positions come from our layout), leafgirl
  12.7 (resvg draws seams between tiles of a bitmap-fill `<pattern>`),
  SimpleText 5.7, WATCH 5.5. Sidecar + minify gives the same numbers.
  Heights differ by one row on some files (resvg rounds the height up).
  Scripts: scratch only; automated since by `cargo xtask export-check`
  (next section).

### Feathers in SVG (XARA-T-0317, 2026-09-24)

A feather's owner (`xarast-format.md`, "Feathers are written on their
owner") carries `filter="url(#f…)"` to a filter derived from the model in
`svg/effect.rs`, in **both** dialects: a `.xarast` opened in a browser
shows its feathers too. The chain, in the owner's user space (points),
with `r` = size/2:

1. `SourceAlpha` moved into the colour channels (`feColorMatrix`, A = 1);
2. eroded by four rectangles `(r cos φ, r sin φ)`, φ = 11.25°, 33.75°,
   56.25°, 78.75° (`feMorphology`), combined with `feBlend mode="darken"`
   (a per-channel minimum over opaque inputs): erosion by a union is the
   minimum of the erosions, and the union reaches 0.98 r–r in every
   direction, i.e. a disc;
3. moved back into alpha, `feGaussianBlur` σ = r/2 (the interchange
   stand-in for the renderer's disc of radius r, `blur::sigma_for_disc_radius`);
4. the profile as a 33-entry `feFuncA` table, `1 − P(1 − a)` (the renderer
   profiles transparency), omitted for the identity;
5. `feComposite in="SourceGraphic" operator="in"`.

`color-interpolation-filters="sRGB"`. Region: `userSpaceOnUse`, the
subtree's geometry grown by twice its widest *visible* stroke, the size
and 1 pt (`Emitter::effect_region`); a placed image with a `transform`
uses `objectBoundingBox` −10 %/120 % and per-axis radii in its unit
square; a subtree with text uses its box ± 50 %. The report counts them
in `Stats::effects_baked` → `Simplified { "feathers drawn by an SVG
filter (a Gaussian blur for the disc blur)" }`; `NotRendered
"feathering"` is left for a feathered text run only.

**Measured** (72 dpi, paper, mean |Δ| against our PNG; Chrome = headless
`google-chrome --screenshot` of the SVG at the PNG's size, scratch
script, the method of `xarast-format.md`):

| File | before | box erosion only | disc union (shipped) |
|---|---:|---:|---:|
| feathers, resvg | 20.15 | 5.06 | **2.00** |
| feathers, Chrome | 20.00 | 4.27 | **1.63** |
| Groucho2, resvg | 5.71 | 3.12 | **2.01** |
| Groucho2, Chrome | 5.71 | 2.56 | **1.49** |
| Watch4, resvg / Chrome | 1.43 / 1.45 | 1.17 / 1.06 | **1.13 / 1.06** |

**Filter, not a raster.** The filter alone is under the default 4 in both
renderers, stays vector (resolution-independent, as the renderer is) and
needs no renderer in `xarast-format`, so the `.xarast` base SVG gets it
too at no cost. No raster-mask fallback was built; if an effect ever
needs one, the hook belongs beside `SvgOptions::derived_bitmaps`
(export-only, supplied by `xarast-io`), and a raster must never go into
the Native dialect (a save does not render). What is left in the
residual: the Gaussian's tails versus the disc's hard support (a thin
feathered line fades a little more in SVG), and the renderer's soft
erosion at antialiased edges.

### Shadows in SVG (XARA-T-0320, 2026-10-15)

A wall or glow shadow's **controller** `<g xarast:kind="shadow">` (it holds
the object as its `live-source` child, so the filter's `SourceGraphic` is
the object) carries `filter="url(#f…)"` to a filter built by
`effect::shadow` from the shadow's parameters, in both dialects. Chain, in
points, `feFlood` first (colour, `flood-opacity` = colour alpha × the
walker's `opacity_level()/255`, i.e. `round(255·(1−(round(255(1−darkness)))/255))`),
then:

1. glow only: `SourceAlpha` dilated by the glow width as a disc (the
   feather's four-rectangle union, `feBlend mode="lighten"` this time);
2. `feGaussianBlur` σ = penumbra/4 (r = penumbra/2, σ = r/2 like the
   feather; skipped at 0);
3. wall only: `feOffset` by the offset in the SVG frame (skipped at 0);
4. the profile as a 33-entry `feFuncA` table, `1 − P'(1 − a)` with the
   bias **negated** (`P'`), as the renderer maps the blurred silhouette;
5. `feComposite in="c" operator="in"` (the flood where that mask is), then
   `feMerge` of it under `SourceGraphic`.

Region: `effect_region(controller, blur + glow + max|offset|)`.
`Stats::shadows_baked` feeds `Simplified { "wall and glow shadows drawn by
an SVG filter (a Gaussian blur for the disc blur)" }`.

**Floor shadows and feathered controllers (XARA-T-0324, 2026-10-15; feathered wall and glow controllers are one filter since XARA-T-0327, below)**: a
derived wrapper `<g xarast:derived="shadow">` with a `<use>` of the source
(floor map `F·M·F⁻¹` as `matrix(…)`) and the shadow-only filter, placed
before the object inside the controller; the reader skips `xarast:derived`
elements (`xarast-format.md`). The filter acts in the wrapper's space, after
the `<use>`'s transform, so the blur is isotropic like the renderer's disc
blur after the warp. Resvg draws it (synthetic test document: squashed,
sheared copy under the object). **No corpus file has a floor shadow** (98
wall, 0 floor, 0 glow, none feathered), so the corpus scores did not move
(Groucho2 svg 6.77, SoftShadow 3.16, 118 comparisons, 0 failures, before
and after). `NotRendered "shadows"` is now reported only for a controller
without a source. Residual: the anchor ignores the line width (up to
half of it times 1 − scale off the renderer's), shadows of text keep the
±50 % region.

**Measured** (72 dpi, paper, resvg, mean |Δ| against our PNG):
Groucho2 6.67 → **2.69**, SoftShadow 5.03 → **3.16**. Whole corpus: 118
comparisons, 0 failures, both SVG limits dropped. What is left: the
Gaussian's tails against the disc's hard support (as for feathers) and the
shadows of text, whose region is the story's box ± 50 %.

**Feathered wall and glow controllers (XARA-T-0327, 2026-10-16)**: the
derived copy made the controller's feather read `SourceAlpha` with the
shadow in it as drawn (blurred, at its darkness), so it eroded most of the
shadow; the renderer cuts that feather from the object's outline and its
moved (wall) or grown (glow) copy, unblurred and opaque (`render.md`,
"Nested effects in a silhouette pass"). Now such a controller carries
**one** filter, `effect::feathered_shadow` (`xarast:filter="shadow"`, so
the reader drops it like any shadow filter): the shadow chain with its
result `s`, `feMerge result="k"` of `s` under `SourceGraphic`, then the
feather's silhouette `u` (wall: `feMerge` of `SourceAlpha` and its
`feOffset` by the shadow's offset; glow: `SourceAlpha` dilated by the glow
disc, which encloses the object), the feather's mask from `u`
(`feather_mask`: disc erosion, blur, profile) and `feComposite in="k"
operator="in"`. `xarast:feather` is still written on the controller; no
feather filter and no derived copy are. `Stats::effects_baked` and
`shadows_baked` count one each, as before. Unfeathered shadows write the
same bytes as before. The silhouette is the object's *alpha*, not an
opaque pass (a filter cannot tell coverage from transparency), so a
transparent object's feather mask is weaker than the renderer's.

Measured on a synthetic document (a feathered wall and a feathered floor
controller, feather 12 pt, offset 8 pt, darkness 0.6; 144 dpi, paper,
resvg against our PNG): wall half 2.69 → **0.68**, floor half 2.47
unchanged. Left (`render.md` TODO 34): a **floor** controller's feather
still reads its derived copy as drawn (no filter applies the floor's affine
map), and a feather on a **group around** a shadow controller reads its
whole `SourceGraphic`, shadow included (a filter cannot separate the
shadow from the object; `feImage` of an element would, but Firefox does not
support it). No corpus file has either (98 wall shadows, none feathered).

**Shadows from rendered alpha (XARA-T-0327)**: the renderer now casts a
wall or floor shadow from its content's rendered alpha, which is what
the shadow filter's `SourceAlpha` always was: Groucho2 svg 6.87 →
**6.81** (its lipstick's highlight), pdf 4.77 unchanged; SoftShadow,
Girard_simple, Watch4 and testimp1 unchanged. A glow is still cast from
an opaque pass by the renderer but from `SourceAlpha` in SVG, so a
partly transparent object glows weaker in SVG (no corpus glows).

## File › Export (T11.1.8, XARA-T-0192, 2026-10-16)

- **The source.** `impl xarast_io::ExportSource for Session`
  (`xarast-app/src/export.rs`); the CLI's `SessionSource` is gone and
  `xarast-cli export`, `fixtures`, `bench` and the tests pass `&session`.
  Areas: drawing = `drawing_or_page_rect`, page = `page_rect` or the given
  page node, spread = `spread_rect`, selection = `EditState::selection_bounds`
  (empty → `ExportError::Area`). Walker shortfalls (`WalkStats`) and font
  substitutions become `NotRendered` / `FontSubstituted` as before. Fonts:
  the session walker's service (`Session::walker_fonts`, `None` = the
  process's shared one) for the scene and `text_as_outlines`.
- **What an export draws is a function of the document only.** The scene is
  walked with a fresh `EditState::for_document` and
  `session::initial_viewport(doc)` (1024 × 768 fitted to the page, what a
  freshly opened session has — so the CLI's bytes did not move), never with
  the user's selection, zoom or tool preview. The viewport matters: the
  walker picks a photo-adjusted bitmap's proxy level from the view's scale
  (`proxy_level_of`). Test `the_export_does_not_depend_on_the_view_or_the_selection`.
- **Off the interface thread.** `Session::export_job(request)` resolves the
  area to `ExportArea::Rect` on the UI thread (node ids do not survive a
  snapshot restore), snapshots the document and carries the decoded-bitmap
  cache and the fonts. `ExportJob::run` restores the snapshot into a
  detached source with the same code (`DocSource`) and calls the registry.
  `ExportWorker` runs one job at a time on `xarast-export`; `ExportProgress`
  (atomics: cancel flag, stage, fraction bits) is the `Progress` the
  exporters poll before every band. Measured cancel latency (debug build,
  1200 dpi A4 PNG, cancel at the first `Render` report): **21 ms**.
- **Cancel leaves no file.** The exporters' atomic writes remove the
  `.part`; a cancel that lands after the last band (the export returned
  `Ok`) makes the worker delete the file it just wrote and report
  `Cancelled`, so "Cancel" always means "no file". Closing the dialog
  while an export runs cancels it.
- **Byte identity with the CLI** (acceptance): `xarast-cli/tests/file_export.rs`
  runs the binary and `AppState` through the dialog's operations on the
  same file and compares bytes — 7 cases (PNG default; PNG 150 dpi, paper,
  page; JPEG default; JPEG q70 72 dpi; WebP; PDF; SVG) × (a synthetic
  `.xarast` made by the test + Groucho2, leafgirl, SimpleText, TextCurve
  from the corpus) = **35 comparisons, all identical**.
  `xarast-app/tests/export.rs` checks a background job writes the live
  session's bytes for all five formats on a synthetic document and six
  corpus files (shadows, bevels, feathers, text on a path, bitmaps, fills,
  stories).
- **The dialog** is generated from the registry's `Capabilities`; a field
  is `None` in `ExportDialogView` exactly when the format does not declare
  it: `!vector` → pixel width/height + "Keep proportions"; `has_dpi` →
  resolution and printed width/height (shown in the document's unit);
  `alpha` → background (Transparent / Paper colour); no alpha → the note
  "JPEG has no transparency: transparent areas are flattened onto the
  paper colour." and no background field (the request keeps
  `Background::Transparent`, as the CLI default, and the report says
  `AlphaFlattened`); `lossy` → Quality 1–100; `max_side` → "WebP stores at
  most 16383 px a side" and Export disabled. Size/dpi/printed size are
  linked by `ExportSizing::edit` (resolution pinned by default, as the
  CLI). Per format: PNG = size, dpi, printed size, background; JPEG = size,
  dpi, printed size, quality, flatten note; WebP = size, background (no
  dpi: WebP stores none); PDF and SVG = background only. After the export
  the dialog shows the summary and every `ExportReport::compromises` line.
  Defaults are the CLI's (PNG, drawing, 96 dpi, transparent, `Final`).
- **Every CLI option is in the dialog now (XARA-T-0350)** except
  `--draft` and PNG `--optimise` (a build feature). Format-independent:
  **Bleed** (`SetBleed`, never negative; the sizing fields resolve against
  the *bled* area, as the exporters do) and the **Rectangle** area
  (`AreaChoice::Rectangle`, `SetRectangle`, document units y up; an empty
  one disables Export). Background gains **Colour** (`SetBackgroundColour`,
  RGBA; offered only where `Capabilities::alpha`, so JPEG still has none).
  Per format the view carries `png` / `jpeg` / `pdf` / `svg:
  Option<…Options>` which is `Some` exactly for the chosen format, and the
  op replaces the **whole** options struct (`SetPng(PngOptions)` …) after
  sanitising (JPEG quality 1–100, PDF raster dpi within
  `PDF_RASTERISE_DPI`, palette 1–256); an op for another format is
  ignored. Options are kept per format while the dialog is open.
- **Byte identity grew**: `xarast-cli/tests/file_export.rs` has 11 cases
  (the 7 above + PNG with bleed, colour background, 16-bit grey,
  interlace, no dpi; PNG with an explicit rectangle and a translucent
  colour; JPEG progressive 4:4:4 with bleed; PDF with raster dpi, native
  blends, no compression, outlines and bleed; SVG minified, outlines, page)
  × 5 files = **55 comparisons, all identical**.
- **Export hints (T11.1.5).** After a *successful* export the app stores
  `ExportDialog::hints()` in `Document::meta.export_hints` (a JSON string:
  version, format, area, rectangle, bleed, dpi, lock_aspect, background,
  colour and the options of every format; keys sorted, so deterministic).
  `ExportDialog::open` restores it leniently (`restore`: anything missing,
  unreadable or invalid keeps its default; a selection area is not
  restored). Storing is **not an edit**: no undo step and the document does
  not become modified; the hints reach disk at the next save. `meta.xml`
  gets `<xarast:export-hints>JSON</xarast:export-hints>` **only when set**,
  so documents without it keep their bytes (`xarast-format/tests/
  export_hints.rs`: no element without hints, round trip, fixed point
  through `save_opened_to`). `xarast-doc` cannot name the export crate's
  types, hence the opaque string. A reader that does not know the element
  ignores it (the element is `xarast:`-namespaced, `meta.xml` is rewritten
  from the model on every save).
- **Proxy level by output dpi: not a bug.** The note that a 300 dpi export
  of a photo-adjusted bitmap might use a reduced proxy was checked against
  `walker.rs`: `proxy_level_of` is reached only from `proxy_image` (a
  slider drag's preview chain) and `standin` (the stand-in of a *deferred*
  derived image, `with_deferred_derived`, which only the session's
  interactive walker enables). The export walker has no preview and no
  deferral, so `derived_image` evaluates the chain at full resolution, and
  a deferred image found in the shared cache is made in place under
  `MissingLevels::Materialise`. `tests/export_proxy.rs` pins it (a
  4000 × 3000 adjusted photo keeps its full size in the export scene). A
  scale parameter on `build_scene` was written, found to change nothing,
  and dropped: do not re-add it unless an export path ever walks with
  previews or deferral.

## Colour fidelity and regression (W11.5, W11.6, XARA-US-0060)

### Colour (T11.5.1–T11.5.5)

- **Markers** (T11.5.1): PNG `sRGB` (or `iCCP`, below), JPEG EXIF
  `ColorSpace = 1`, PDF `DeviceRGB`, SVG CSS sRGB. **WebP carries no
  `ICCP`** on purpose: the container spec makes a chunk-less WebP sRGB, and
  embedding a profile would cost ~3 KiB a file to restate it (and we have
  no sRGB profile of our own yet; generating one is also what the PDF
  output intent of XARA-T-0230 needs). The table is the module doc of
  `fidelity.rs`; `tests/fidelity.rs` (CLI) asserts each marker.
- **ICC pass-through** (T11.5.2): SVG passes PNG/JPEG/GIF originals byte
  for byte (profile included); a bitmap SVG must re-encode (a `.xar` JPEG
  with a reconstruction palette, BMP flavours) gets the decoder's profile
  back through `png::with_icc_profile`, which swaps `sRGB` for `iCCP`
  right after `IHDR` (PNG allows one of the two). Correct because the
  renderer never converts pixels (`to_working_space` is phase 15). Raster
  and PDF output draw such images as sRGB → **`Compromise::ProfileDropped
  { images }`**. Detection: a header probe of the original
  (`fidelity::has_icc_profile`); the document's `BitmapInfo` records no
  colour space.
- **CMYK and spot** (T11.5.3): no output profile exists, so every format
  gets the naive conversion the renderer uses, and every report says so:
  **`Compromise::ColourConverted { model: "CMYK" | "spot", count }`**,
  counted per fill/line colour *attribute* (a gradient with two CMYK stops
  is one). Spot = `ColourKind::Spot` or anything derived from one (tint,
  shade, link). No `icc-color()` in SVG and no `DeviceCMYK`/`DeviceN` in
  PDF: research/06 §6.12.3 makes `icc-color()` conditional on a profile.
  Note for a future PDF `DeviceCMYK` mode: PDF's own DeviceCMYK→RGB rule
  (`1 − min(1, C + K)`) is exactly our conversion, so a flat CMYK fill
  written as `k` would look identical on screen and keep its separations.
  It needs the CMYK value to survive into the display list (it does not
  today: the walker resolves to `Rgba8`).
- **Report** (T11.5.4): `fidelity::document_compromises(src, Target)` is
  appended by all five exporters (`raster::finish`, `PdfExporter::export`,
  `SvgExporter::export`); empty for scene-only sources.
- **Colour sheet** (T11.5.5): `xarast_cli::fixtures::colour_sheet` — 20
  flat patches (RGB, CMYK, HSV, grey, a named CMYK palette colour, its
  palette tint, a local tint, a spot ink) with `colour_patches()` giving
  each patch's rectangle and expected bytes (`resolve(table).to_rgba8()`,
  the walker's and the SVG writer's rule). `crates/xarast-cli/tests/
  fidelity.rs` reads all five formats back: PNG/WebP exact over 5×5 px at
  every centre, JPEG ±6, SVG `<rect>` fills and uncompressed-PDF `rg`
  operands exact in document order, Poppler's render ±1. Hand-written
  anchors pin the model (`cmyk(0.2,0.3,0.4,0.1)` → `#b39980`).

### Regression harness (T11.6.1–T11.6.4)

- **`xarast-cli fixtures --out-dir D --format F [export options]`**
  exports three built-in documents through `Session` (the export source) and the
  registry (`export::run_on` takes in-memory inputs): `colour-sheet`,
  `features` (linear gradient with a middle stop, elliptical radial, 8 pt
  stroke, 50 % flat transparency over an opaque square), `synthetic`
  (2 000 nodes of `xarast_doc::synth`). **No text**, so the bytes do not
  depend on installed fonts.
- **`cargo xtask export-check [--require-tools] [--limits F] DIR`**: for
  each `<stem>.png` (our export, 72 dpi, paper) renders `<stem>.svg` with
  resvg and `<stem>.pdf` with **both Poppler and Ghostscript** at the
  PNG's exact size, and judges mean |Δ| (1/255, RGB over white) against 4
  or the per-file limit; `qpdf --check` on every PDF; fails on `xarast:`.
  A PDF is judged on the renderer that agrees best, but a renderer that
  *fails to read* it fails the check.
- **CI** (`ci.yml`): job `export` on ubuntu-24.04 and ubuntu-22.04 —
  fixtures in five formats twice (`diff -r`), `svg-check --interchange`,
  `export-check --require-tools`, a SHA-256 manifest uploaded; job
  `reproducible` diffs the two manifests (T11.6.4 across machines). The
  `check` job installs Poppler, so `tests/pdf.rs`'s render comparison and
  the colour sheet's Poppler test now run in CI. Nightly
  `export-corpus.yml`: sparse checkout of the fork's corpus directories
  (secret `CORPUS_TOKEN` if the fork is private), corpus tests, the 59
  files to PNG/SVG/PDF, `export-check` with `export-limits-corpus.txt`.
- **T11.6.1** was already the CLI corpus test (all five formats, 0
  failures); T11.6.4 on one machine is `the_fixtures_are_byte_identical_
  across_processes`.

### Measured (2026-09-24, 72 dpi, paper)

Fixtures: colour-sheet svg 0.49 / pdf 0.50; features 0.23 / 0.87;
synthetic 1.65 / 3.53 (gs; Poppler 8.9). Corpus (re-measured after
XARA-T-0252): 59/59 each format; SVG median 0.70, PDF best-of-two median
1.98; **15 file/format pairs above 4** (1 SVG: leafgirl; 14
PDF; SoftShadow svg left the list with XARA-T-0320), each with a limit and a reason in `export-limits-corpus.txt`:

| Cause | Files (format: mean) |
|---|---|
| Bake ladder: conical/diamond/multi-colour fills | Fill Types simple (~~svg 19.7~~ 1.9 since XARA-US-0043, pdf 7.2), WATCH2 ~~svg 16.5~~ 0.7, WATCH (~~svg 5.1~~ 2.4, pdf 4.8) — SVG now bakes them into geometry (`xarast-format` `svg/bake.rs`, `xarast-format.md`) |
| Bitmap fills: resvg tile seams / PDF rasterised + resampled | leafgirl (svg 11.4, pdf 7.7), TestBitmapFill pdf 5.6 |
| ~~Feathers drawn by us, unfeathered in SVG~~ | feathers svg ~~20.15~~ **2.00**, Groucho2 svg ~~5.71~~ **2.01**, Watch4 1.43 → 1.13 since XARA-T-0317 (the feather is a filter, "Feathers in SVG" below); the limits 23.2 / 6.6 are gone. PDF rasterises each feather whole: feathers pdf 1.69, Groucho2 2.04 |
| ~~Shadows drawn by us, not baked in SVG~~ (XARA-T-0318, 2026-09-24; baked by XARA-T-0320, 2026-10-15) | Groucho2 svg 5.71 → 10.04 (feathers unfiltered too) → 6.67 with the T-0317 feather filter merged (only the 81 shadows missing) → **2.69** with the shadow filter ("Shadows in SVG" below); SoftShadow svg 1.20 → 5.03 → **3.16**; the limits 7.7 / 5.8 are gone. PDF rasterises each shadow with its object over its growth: Groucho2 pdf 2.75, SoftShadow pdf 7.56 (gs; Poppler 8.11: its controllers hold the whole textured page, resampled by the viewers; limit 8.7), Girard 1.05, Watch4 0.80, testimp1 0.84. Corpus export check: 118 comparisons, 0 failures (qpdf not installed locally) |
| Bevels (XARA-T-0322, 2026-10-15): Groucho2's frame, stripped with its 21 bevels until now, drawn | Groucho2 svg 2.69 → **6.77** (6.88 with bevels unbaked; the bevel filter at `surfaceScale` 1 × indent made it 7.97, at 0.25 × 6.77–6.83), pdf 2.75 → **4.64** (gs; Poppler 7.10): the frame's seven JPEG textures render differently in resvg / the viewers (as the bitmap-fill row), PDF rasterises each bevel whole. Limits 7.8 / 5.4 with the reason. testimp1 svg 0.94 unbaked → **0.49** baked, pdf 0.94. Corpus export check: 118 comparisons, 0 failures |
| Bevel bake in user units (XARA-T-0328, 2026-10-16): the `feDiffuseLighting` chain was resolution dependent and ignored the joins and the inner flat paint; now a ramped height field, an offset difference along the light in whole user units, the renderer's own shading, the outer join on rectilinear sources and the inner flat paint (`xarast-format.md`, "Bevels") | measured at **72 and 144 dpi** (paper, resvg vs our PNG at that dpi; `cargo xtask export-check` on a directory with `xarast-cli export --dpi D`): Groucho2 svg 6.81 → **5.72** (72), 4.89 → **3.94** (144); testimp1 svg 0.69 → **0.59** (72), 0.36 → **0.32** (144) with the lighting, then → **0.56** / **0.28** with the mitre join on its rectangular frame (a square structuring element). A six-shape synthetic document (inner and outer, round, rectilinear and triangular sources, a painted inner band, a mitre) 4.35 → 2.63 (72), 3.66 → 2.09 (144); face colours of the same 12 pt chamfer at the two dpis agree with each other within 7/255 (the first user-unit version, with fractional offsets, had the lit face's green at 171 at 72 dpi against 185 at 144: resvg truncates `feOffset` to whole device pixels). The scores fall with the dpi for the same reason as every row here (a pixel of antialiasing is a smaller share). Limit Groucho2 svg 7.8 → **6.6**. Corpus export check: 118 comparisons, 0 failures |
| Contours (XARA-T-0330, 2026-10-15): testimp1's outer contour and Girard_simple's three inner ones, stripped until now, drawn and baked into SVG as real offset paths | measured against the commit before (d96c03d), 72 dpi, paper: testimp1 svg 0.52 → **0.58**, pdf 0.89 → **1.05** (gs; Poppler 1.09 → 1.16); Girard_simple svg 0.28 → **0.32**, pdf 1.05 → **1.09**. The rise is the new ink (four stroked rings) antialiased by three rasterisers, not a disagreement in shape. No limit changes. Corpus export check: 118 comparisons, 0 failures |
| Blends (XARA-T-0333, 2026-10-15): the 480 corpus blends, opaque until now (their end objects drawn, no steps), drawn and baked into SVG as real paths | measured against the dev branch before (314f943), 72 dpi, paper, our PNG vs our SVG (resvg) / PDF (best of Poppler, gs): BLUECAR svg 0.73 → **0.73**, pdf 2.19 → **2.19**; ProbeX16 0.91 → **0.92**, 4.38 → **4.37**; WATCH 2.07 → **2.05**, 5.36 → **5.30**; WATCH2 0.72 → **0.74**, 2.69 → **2.58**; Watch4 0.75 → **0.72**, 0.97 → **1.03**; leafgirl 9.51 → **9.48**, 9.39 → **9.35**; scope3 simple 2.23 → **2.30**, 2.83 → **2.99**; testimp1 0.58 → **0.64**, 1.05 → **1.23**. The exports agree with our raster as before: the steps are ordinary paths. Visibly, WATCH2 and Watch4 gain their bracelet links, hands, rims and bows. No limit changes. Corpus export check: 118 comparisons, 0 failures; `svg-check --interchange` 59/59 |
| Moulds (XARA-T-0334, 2026-10-16): the corpus's 197 moulds, stripped until now, drawn and baked into SVG | measured against the blends base (ac780c4), 72 dpi, paper: Watch4 svg 0.72 → **0.91**, pdf 1.03 → **1.24** (gs; Poppler 1.62 → 1.83); TextCurve svg 3.06 → **3.20**, pdf 3.46 → **3.84** (Poppler; gs 5.80); testimp1 svg 0.64 → 0.69, pdf 1.23 → 1.35; scope3 simple svg 2.30 → 2.31, pdf 2.99 → 3.02; ProbeX16 svg 0.92 → 0.92, pdf 4.37 → 4.40. New ink (moulded text as curves, perspective gradients) antialiased by three rasterisers. No limit changes. Corpus export check: 118 comparisons, 0 failures; svg-check 59/59. Preview |Δ| (registered 60 dpi page render, same scratch scorer both sides): Watch4 7.17 → **4.58**, TextCurve 13.53 → **3.21**, testimp1 9.61 → **6.55**, scope3 15.64 → 15.65, ProbeX16 8.97 → 8.95 |
| Contour follow-ups (XARA-T-0332, 2026-10-16): rings refitted to cubics (≤ 187.5 mp from the polyline), outer contours bounded by their rings, an inset path's source `display="none"`, text and nested output contoured | measured against fa3b3f2, 72 dpi, paper: testimp1 svg 0.56 → **0.56**, pdf 1.35 → **1.35** (Poppler; gs 1.40); Girard_simple svg 0.50 → **0.51**, pdf 1.67 → **1.67** (Poppler; gs 3.90). Preview |Δ| (60 dpi): testimp1 6.55 → 6.55, Girard_simple 5.22 → 5.22. Sizes: Girard_simple SVG export 2 539 160 → **2 507 949 B**, `.xarast` document.svg 2 688 732 → 2 657 521 B, package 361 846 → **352 304 B** (−2.6 %); testimp1 (one small mitred ring set) SVG 23 306 → 23 399 B. The rings are the same shapes within a quarter pixel, so no score moves. No limit changes. Corpus export check: 118 comparisons, 0 failures; svg-check --interchange 59/59 |
| Fractal and noise fills (XARA-T-0336, 2026-10-16): the corpus's 196 clouds/plasma fills, flat midpoint colours until now, drawn from their generated tiles and baked into SVG as coloured PNG tiles | measured against the blends base (ac780c4, before the moulds merge), 72 dpi, paper, our PNG vs SVG (resvg) / PDF (best of Poppler, gs): amurdove svg 1.24 → **1.21**, pdf 1.98 → **2.18**; Fill Types simple svg 1.82 → **2.60**, pdf 7.11 → **8.55** (Poppler; gs 16.14; limit 8.5 → 9.8 with the reason); Groucho2 svg 6.77 → **6.87**, pdf 4.64 → **4.77**; leafgirl svg 9.48 → **9.66**, pdf 9.35 → **9.64**; scope3 simple svg 2.30 → **3.25**, pdf 2.99 → **3.01**; SimpleSphere svg 1.02 → **1.03**, pdf 1.78 → **2.09**; Spitfire svg 2.50 → **2.77**, pdf 3.48 → **3.49**; Watch4 svg 0.72 → **0.84**, pdf 1.03 → **1.11**. The rise is texture detail resampled by three rasterisers. A first version baked the grey tile behind the contone filter: resvg dropped the filter at large pattern scales (amurdove svg 20.64, leafgirl 20.75), so the tile is baked coloured. Sizes grow: SVG (interchange, data: URIs) Spitfire 1.32 → 5.37 MB, Watch4 1.21 → 4.25 MB; PDF (rasterised fills) Watch4 24.1 → 38.9 MB, scope3 simple 55.1 → 60.4 MB, SimpleSphere 0.79 → 3.36 MB |
| ~~Feathering as a blur~~ | SoftShadow ~~svg 4.1~~ **1.20** since XARA-T-0256: the excess was its four-colour Bleach transparencies, drawn flat; they are baked as masks now (`render.md`, "Meshes tile mirrored"); the limit is gone |
| ~~Text on a path written straight in SVG~~ | TextCurve ~~svg 16.3~~ **3.06** since XARA-T-0252 (T9.5.6: each character placed and turned on the path, `x`/`y`/`rotate`), pdf 1.55; the limit is gone. What is left is the rainbow gradient on the text, written in the spread frame (`xarast-format.md`, text leftovers) |
| Small text (5–9 px glyphs) as filled outlines | TextJust 13.7, ScaleTest2 9.6, SimpleText 8.6, ScaleTest 7.3, Paragraph 6.7, FontChangesInText 6.7, SuperSub 5.8, Rotated 5.5, ManualKern 5.0, Tracking 4.7, ProbeX16 4.2 (all pdf) — superseded by the row below since T11.4.7; `--text outlines` still gives these |
| Small text as live text in embedded fonts (T11.4.7, re-measured 2026-09-24) | TextJust 18.4, SimpleText 13.3, Paragraph 10.9, ScaleTest2 9.4, FontChangesInText 9.4, SuperSub 8.9, embeddedFonts 8.6, ManualKern 7.7, ScaleTest 7.1, Rotated 7.0, Tracking 6.6, GardenPlan 6.1, hebrew 5.8, BaselineShift 5.5, ProbeX16 4.4, Kerning 4.1 (all pdf; limits = +15 %, LineSpacing 3.97 and AngledText 3.66 given headroom too) |

Corpus after the fonts round (system fonts, release): 59/59 each format,
SVG median 0.70 (unchanged; hebrew.svg 0.55), PDF best-of-two median 2.19;
`export-check --limits` passes (0 failures) with the limits above. All 59
PDFs open in Poppler and Ghostscript with no message; 20 carry fonts, 29
fonts in all, every one embedded, subset and with `ToUnicode` (`pdffonts`).
Two runs are byte-identical (32 text-file PDFs and SVGs compared).

After merging the image resampler (XARA-US-0052) with font embedding
(2026-09-24, release, 72 dpi, paper): 59/59 each format, SVG median 0.74,
PDF best-of-two median 2.37, 0 failures. The merge's PNGs are
byte-identical to the resampler branch's and its SVGs to the font
branch's; the resampler alone already failed three limits (SimpleText
svg 6.20, TextJust svg 9.17, leafgirl pdf 9.38), all from the PNG side:
the TextDesigns files place the original's own text rendering as a
bitmap minified 2-4x, which our PNG now filters with a widened tent or
linear-light mips and resvg/the viewers do not. New limits and reasons in
`export-limits-corpus.txt` (leafgirl svg lowered to 11).

Small text: Poppler and Ghostscript both paint thin glyph features darker
than our coverage AA; rendered at 8× and compared at 8×, TextJust's PDF is
within 2.4/255 of our 8× export, so the outlines are right. Poppler also
strokes anything under a pixel as a whole pixel (synthetic 8.9) and
mis-strokes the degenerate butt cap of `Broken Butt Cap` (12.6 where
Ghostscript gives 0.97) — the reasons for a second renderer.



### The T11.4.1 spike: `pdf-writer`, not `krilla`

The phase recommended `krilla` and named `pdf-writer` as the fallback. The
spike read `krilla` 0.8.2's scene model against our feature list and chose
the fallback:

| Criterion (phase order) | `krilla` 0.8.2 | `pdf-writer` 0.15 |
|---|---|---|
| Pure Rust | yes | yes |
| Licence | MIT OR Apache-2.0 | MIT OR Apache-2.0; deps `bitflags`, `itoa`, `memchr`, `ryu` |
| A baked ramp as a sampled function (type 0) | **no**: gradients are stop lists, written as stitched exponentials or PostScript (type 4) functions | yes |
| Mesh shadings (types 4–7) | **no** | yes |
| Soft masks, groups, blend modes | yes | yes |
| Shares our types | **no**: `tiny-skia-path` geometry in `f32`, not `kurbo`; pins `skrifa` 0.42 (we have 0.44) | n/a (we pass numbers) |
| MSRV | **1.92** (workspace declares 1.90) | fine |
| Font subsetting | built in (`subsetter`) | not included — T11.4.7 adds `subsetter` directly |

So the phase's premise ("same `kurbo`/`skrifa` family, no conversion
layer") does not hold for 0.8, and `krilla` cannot carry the two things
the ladder most needs (the sampled ramp and the meshes). The façade in
`pdf/writer.rs` keeps the choice replaceable.

### The fidelity matrix (what each feature becomes)

| Feature | PDF | Rung |
|---|---|---|
| Path fill, non-zero / even-odd | `f` / `f*` | native |
| Fill rule Positive / Negative | `f` (non-zero) | native: the original and the renderer draw them non-zero (`FillRule::as_drawn`, XARA-T-0331; they were rasterised before) |
| Clip, non-zero / even-odd | `W n` / `W* n` inside `q`/`Q` | native |
| Clip, Positive / Negative | `W n` (non-zero) | native, as above (was reported as approximated) |
| Stroke: width, join, mitre, one cap, dashes + offset | `w J j M d`, drawn in document units under the object's transform (`cm`), coordinates relative to the path's corner | native |
| Hairline (width 0) | `0 w` | native (device-dependent by definition) |
| Different start and end caps | outline via `kurbo::stroke`, filled | workaround, exact |
| Gradient- or bitmap-painted stroke | outline, then as a fill | workaround |
| Flat colour, colour alpha, flat Mix transparency | `rg` + `ca`/`CA` | native |
| Linear, radial (elliptical, skewed) | axial / radial shading in the gradient's frame (`cm` to A/B/C) | native |
| Diamond | `\|u\|` axial everywhere + `\|v\|` axial clipped to the bow tie where `\|v\| > \|u\|` | native, exact |
| Repeat / mirror / repeat-HQ | domain widened to whole periods, parameter folded while sampling (≤ 4096 periods, else rasterise) | native |
| Ramp profile and effect space | the renderer's 256/2048 table as a sampled function | native, exact up to sampling |
| Conical | 256-wedge Gouraud fan (type 4) | **approximated** (reported) |
| Four-colour mesh | type 1 shading of a 2 × 2 sampled function; the function clamps its input as the renderer clamps (u, v) | native, exact |
| Three-colour mesh | type 1 shading, 33 × 33 grid of the renderer's formula | **approximated** (reported) |
| Perspective gradient mapping | — | **rasterise** |
| Ramp or mesh colours with alpha | — | rasterise (XARA-T-0227) |
| Graduated transparency (Mix) | luminosity soft mask painting `255 − t` with the same shading code | native |
| Bitmap transparency | — | rasterise (XARA-T-0227) |
| Families other than Mix | `Exact` (default): **rasterise with backdrop**. `PreferNative`: Stained Glass → Multiply, Bleach → Screen (reported as `BlendModeApproximated`), the rest rasterised | see below |
| Layers (`PushLayer`) with flat Mix | transparency-group form XObject, `/I` unless `Plain`, `ca` on the `Do` | native |
| Layers with other transparency | rasterise the whole layer with backdrop | rasterise |
| Bitmap fills, placed images | — | rasterise (XARA-T-0229) |
| Text, flat fill | `BT 0 Tr … Tj ET` in an embedded subset `CIDFont`, one `Tm` per glyph | native (T11.4.7) |
| Text, gradient fill | glyphs as a text clip (`7 Tr`), then the shading | native |
| Text the ladder cannot paint as text (bitmap fill, rasterised transparency, variable instance, underline under non-opaque paint) | the outlines (or pixels) as before, plus the glyphs as invisible text (`3 Tr`) | selectable |
| Text in a face whose `fsType` forbids embedding | the walker's glyph outlines, no text | reported `FontNotEmbedded` |
| Underline | filled rectangles next to the text | native |
| Live effect: feather, shadow, bevel (`PushEffect`) | the effect rendered whole, alone, over its content **plus its growth** (a shadow reaches beyond what it wraps; XARA-T-0318), reason names the effect | rasterise |

**Blend families.** Not measured yet (T11.4.6, XARA-T-0227): so `Exact`
is the default and every non-Mix family is rasterised with its backdrop.
`PreferNative` maps only the two per-channel families whose shape is
Multiply/Screen; Darken/Lighten/Hue/Saturation/Luminosity are *not*
mapped to PDF's same-named modes (different formulas, the phase's risk
row), nor are Contrast, Brightness, Bevel.

### How it works

- **Page.** `plan_page`: the (bled) area, sized to `ExportSizing`'s
  physical size, or the area's own size (a pixel-count overflow falls back
  to the area — pixels mean nothing to a vector page). ≤ 14 400 pt a side
  (PDF 1.7 Annex C). `to_page` maps millipoints to points, y up; TrimBox
  and BleedBox are written when there is a bleed. The page is a DeviceRGB
  transparency group.
- **Translation.** `DisplayList::build` with the page as the "device"
  (points, y up, viewport = the page) — the same resolution of groups,
  transforms and paint mappings the rasterisers use; every `DrawItem` walks
  the ladder. Paint mappings arrive in page space, so a gradient's frame is
  `[C−A, B−A, A]`.
- **Rasterising** (`pdf/rasterise.rs`). A second display list is built for
  the image's pixel grid over the object's bounds and commands are picked
  by **scene op index** (`DisplayList::op_of`), so the pixels are the raster
  exporter's. *Object* mode draws the command alone, twice — over opaque
  black and opaque white — and unmixes coverage and colour. That worked
  around XARA-T-0231, which is fixed (2026-09-23): a single render over
  transparency and `unpremultiply_rgba_in_place` would now give the same
  pixels at half the cost; the switch is left for a PDF task. *Backdrop* mode draws every command up to it, closing open
  clips and layers (`PopLayer { layer: u32::MAX }` composites opaquely;
  the group's own opacity is applied by the PDF group around the image),
  over the **paper** when the page is transparent: a blend reads a colour,
  and that is what the editor showed under it. Such a rectangle is opaque
  in the file (the report says "over the paper").
- **Determinism.** Object numbers in call order; `BTreeMap`/`BTreeSet`
  for interning and resource dictionaries; zlib level 6 through the
  workspace's `flate2`; no `CreationDate`, no `/ID`. Functions and
  shadings are interned by value, which took the corpus from 214 MB to
  121 MB (`20000GradFilledShapes` 20 MB → 3.2 MB).

### Validation (2026-09-23)

- `crates/xarast-io/tests/pdf.rs`: all 120 synthetic cases export twice
  byte-identically, compressed and not, and pass a no-tool structural check
  (xref offsets, `startxref`, catalog); MediaBox = area to 1/1000 pt; the
  criterion-10 sheet gives exactly three `Rasterised` entries naming nodes
  100/101/102; bleed boxes; cancellation leaves no file.
- Render comparison against our own PNG export (72 dpi, paper) through
  Poppler's `pdftoppm` when it is on the `PATH`: every vector case has mean
  |Δ| < 4/255 and < 3 % of pixels off by more than 48. Worst: conical
  3.6/255 (Poppler does not antialias clipped mesh shadings), diamond 2.2.
  Rasterised cases are excluded (Poppler resamples placed images; the
  pixels are ours by construction) and the hairline case too.
- The 59-file corpus (`--background paper`): 59/59 exported, 121 MB,
  6.5 s render in release; Poppler and Ghostscript read all 59 with no
  message. `qpdf` is not installed on this machine (XARA-T-0230).

## Fonts (T11.3.4, T11.4.7, T9.6.5; XARA-T-0228/0233/0245, as built)

One embedding layer for every format: `xarast_text::embed`
(`docs/memory/text.md`, "Font embedding": the `fsType` table, the
subsetter, our WOFF2 writer). This section is what the exporters do
with it.

- **The glyphs behind the outlines (PDF).** The scene only holds
  outlines, so `SourceScene` gained `text: Option<SceneText>`: per painted
  run, its outline `PathRef` (the very allocation the `Fill`/`Stroke` ops
  hold), its `ExportGlyph`s (face, glyph id, font-units → document
  transform with faux italic, whether a variable instance, the cluster's
  text on its first glyph) and its underline. The walker fills it
  (`SceneWalker::scene_text`); `SceneSource` gives `None` (text stays
  outlines). The translator finds a run from an op by the **address of
  the path's `Arc<Path>`** — no render-crate change, and a display list
  built from the scene points at the same allocation.
- **Fonts first, then the page.** `PdfText::new` subsets every face the
  runs use once, with every glyph any run draws, in order of first use
  (not `FaceId` order, which depends on what the process laid out
  before), and writes `Type0` → `CIDFont` (`CIDFontType2` + `FontFile2`
  with `Length1`, or `CIDFontType0` + `FontFile3 /CIDFontType0C`),
  `Identity-H`, `W` widths, a descriptor (`Flags` symbolic, italic, fixed
  pitch; `StemV` 80, not in fonts) and a `ToUnicode` CMap (CID → the
  cluster text of the first run that used the glyph). Subset tag: six
  letters from an FNV hash of the program, so the same subset has the
  same name.
- **Painting.** One `Tm` per glyph (em square → page), `Tf` size 1 only
  when the font changes: no advance arithmetic of the viewer's can move a
  glyph. Flat fill → `0 Tr`; gradient → `7 Tr` then the shading code in a
  scratch canvas (a second coat for the underline's clip); anything else
  the ladder already handles (bitmap fills, rasterised transparency),
  a variable instance, or an underline under translucent or blended
  paint (glyphs and bar painted apart would double up where they touch)
  keeps its old path and gets the glyphs once as **invisible text** so
  it stays selectable; a stroke-only run too. A refused face is never
  written: outlines, `FontNotEmbedded` once per family.
- **SVG.** The writer embeds (see `xarast-format.md`, "Embedded fonts");
  the exporter asks the source for `text_as_outlines(false)` first — a
  copy of the document where stories drawn with a refused face are
  converted to shapes by the application (`xarast_app::convert::
  text_as_outlines`, the convert-to-shapes geometry, so they render
  exactly as the text did) — and reports those families.
- **`TextOutput`** (`PdfOptions::text`, `SvgOptions::text`, CLI
  `--text text|outlines`, serde `text`/`outlines`): `Outlines` ignores
  `SceneText` in PDF and converts every story in SVG (no `<text>`, no
  `@font-face`, no `FontNotEmbedded`). TextDesigns + TextCurve with
  `--text outlines`, system fonts: SVG 0.55–2.17 (TextCurve 0.57, was
  3.06 as live text), PDF the old outline numbers.
- **`Capabilities::embeds_fonts`** is true for PDF and SVG.
- **Extraction** (`pdftotext`): every story's text comes back; manual
  kerns and wide tracking read as spaces ("l i n e s") because Poppler
  infers word breaks from gaps — the text itself is right.

## Decisions taken (and why)

- **`.xar` export is permanently out of scope** (architecture §3.5; the
  phase-11 banner). Enforced three ways: `Registry` has no `.xar` entry and
  `for_extension("xar")` is `None` (test
  `registry::tests::xar_is_never_an_export_format`); `FormatId` has no `Xar`
  variant; `xarast-cli export --format xar …` and `-o out.xar` exit non-zero
  printing `XAR_EXPORT_REFUSAL`, which names §3.5 (CLI test
  `export_refuses_xar_with_the_architecture_reason`). Re-adding it means
  first reversing the decision in `docs/10-architecture.md`.
- **The export model lives in `xarast-io`, not `xarast-app`.** The phase
  document put `ExportRequest` and friends in the app, but `xarast-app`
  depends on `xarast-io`, so the `Exporter` trait could not name them from
  there. The types need only `xarast-doc` (`NodeId`), `xarast-geom` and
  `xarast-render`, so they sit with the trait. What does need the app —
  turning a selection or a page into a rectangle and walking the document
  into a scene — is the `ExportSource` trait, which `Session` implements
  (XARA-T-0192).
- **Export geometry is exact.** `export_transform` maps the area's top-left
  to pixel (0, 0) and its bottom-right to (w, h), independently in x and y
  (non-uniform when the aspect lock is off). The headless `render` path
  frames with a margin and centres, so the two differ by sub-pixel shifts
  (mean |Δ| 0.17/255 on `10000GradFilledShapes`); that is expected, not a
  bug. A vertically flipped comparison gives 42/255, which is how the y
  flip was checked.
- **Sizing.** `pixels = physical × dpi` in `f64` on millipoints, pixel
  counts rounded half away from zero (`f64::round`), never below 1. One
  axis is pinned; an edit of a non-pinned axis moves the third; an edit of
  the pinned axis moves its default follower (dpi → pixels, pixels → dpi,
  physical → pixels). A physical size of zero means "the area's own size".
  100 mm at 300 dpi = 1181 px; the 20-triple table is in `model.rs`.
- **Background.** `Transparent` keeps alpha where the options do
  (`FormatOptions::keeps_alpha`: PNG RGBA/grey-alpha/palette, WebP);
  otherwise the surface is cleared to the paper colour (a translucent
  explicit colour is composited over the paper first) and the report gets
  `Compromise::AlphaFlattened`. The export renders *onto* that colour
  rather than flattening afterwards, because blend modes over white and
  over transparency differ, and the former is what the user sees.
- **Straight alpha in image files (XARA-T-0231).** Render surfaces are
  premultiplied; PNG and WebP store straight colour. `raster.rs` converts
  every strip with `xarast_render::unpremultiply_rgba_in_place` before
  encoding (a fully transparent pixel becomes `[0, 0, 0, 0]`), and
  `render_export_strips` premultiplies `ExportJob::background`, which is
  straight. Before, premultiplied bytes were written as straight and every
  partly transparent pixel came out darkened by its own alpha, on top of
  the compositor's own darkening. Guarded by
  `tests/raster.rs::a_half_transparent_square_exports_straight_not_darkened`.
- **One rasteriser, CPU only.** `render_export_strips` constructs
  `CpuBackend::new(CpuConfig::deterministic())` itself; it takes no backend
  argument, so the GPU cannot leak in.
- **Band and strip geometry are functions of the output size only.**
  `export_band_lines(w, h)` aims for 64 bands (≥ 16 rows, ≤ 1 MiB), and
  strips (`export_strip_lines`, 64 MiB budget) are whole numbers of bands
  starting on the band grid, so strips change memory and never pixels.
  This is what fixed the parallelism half of XARA-T-0038: a 766 px image is
  48 bands, not 3.
- **PNG is our own writer**, not the `png` crate: `png` 0.18 cannot write
  Adam7 data (it only sets the IHDR flag), compresses with `fdeflate` instead
  of the workspace's one DEFLATE backend, and cannot stream. Ours writes
  IHDR, `sRGB` (perceptual), `pHYs`, `PLTE`/`tRNS`, IDAT in 256 KiB chunks
  and IEND; filters are chosen per row by minimum sum of absolute signed
  bytes. Non-interlaced, non-palette PNGs stream strip by strip.
- **Parallel DEFLATE, deterministic.** `deflate::ChunkedZlib` cuts the
  filtered stream at fixed 1 MiB offsets of *uncompressed* data, compresses
  each piece independently (sync flush; the last piece carries the final
  block) and combines the Adler-32s. Cut points never depend on strips,
  batches or threads. Cost: the lost dictionary at each cut (< 1 %). Level 6
  was ~4× the rasterising time before this.
- **JPEG: `jpeg-encoder`** (quality, progressive, 4:4:4/4:2:2/4:2:0, JFIF
  density, APP segments), `simd` feature **off** (it would pick AVX2 at run
  time). Every file carries an APP1 EXIF block with `ColorSpace = 1` (sRGB),
  44 bytes, built in `jpeg::exif_srgb`. **Standard Annex K Huffman tables**,
  not optimised ones: see dead ends.
- **WebP: lossless only**, through `image-webp` (pure Rust, already in the
  graph). Opaque images are written as RGB. No `ICCP` chunk: WebP without
  one is sRGB. `WebPMode::Lossy` returns `FeatureNotBuilt`.
- **`oxipng` is an optional feature** (`xarast-io/oxipng`, forwarded as
  `xarast-cli/oxipng`), off by default. It builds libdeflate from C, which
  the default AppImage does not want. `parallel` is off so its trial order,
  and therefore its output, is fixed; `StripChunks::None` keeps our `sRGB`
  and `pHYs`. It runs on its own thread; cancelling **abandons** it (the
  call returns within 20 ms, the worker finishes and drops its result),
  because `oxipng`'s own `timeout` would make the bytes depend on the clock.
- **Atomic writes.** Every exporter writes `.<name>.<pid>.part` next to the
  destination and renames it into place; a failure or cancellation removes
  the temporary. Test: `a_cancelled_export_leaves_nothing_behind`.
- **Defaults and why.** PNG: RGBA, 8-bit, not interlaced, `pHYs` written,
  zlib level 6, no `oxipng` (lossless, alpha, the common case). JPEG: q90,
  baseline, 4:2:0, density written (what every viewer expects; q90 is the
  usual "high"). WebP: lossless (the only mode built). CLI: 96 dpi (one
  pixel per screen pixel at 100 %), transparent background, the drawing.
- **Licences.** `jpeg-encoder` is `(MIT OR Apache-2.0) AND IJG`; IJG is
  allowed for that crate only in `deny.toml`, and its acknowledgement
  sentence is in `docs/11-licensing-and-clean-room.md` §4.1 (shipping it in
  the About box and AppImage notices: XARA-T-0194). `image-webp` is
  `MIT OR Apache-2.0`; `oxipng` is MIT, `libdeflater`/`libdeflate-sys` are
  Apache-2.0 (libdeflate upstream MIT). `oxipng` adds a second `rustc-hash`
  to the graph (a `cargo deny` duplicate warning, not an error).

## Invariants that must not be broken

- **Test scratch directories are per process.** The `xarast-cli` test
  helpers (`scratch`, `write_temp`) put the process id in their
  `/tmp/xarast-*` path. With fixed names, two `cargo test` runs at once
  (parallel worktrees) wiped each other's output mid-export. That caused
  the old "SoftShadow.xar: No such file" flake in
  `corpus_exports_to_every_format_with_no_failure`.
- **Determinism contract**: same `ExportRequest` + same document → same
  bytes, across runs, processes, thread counts and strip budgets.
  Guarded by `xarast-render/tests/export.rs` (runs, strip budgets, 1 vs 8
  threads, over the whole synthetic corpus),
  `xarast-io/tests/raster.rs::every_format_is_byte_identical_across_runs`,
  `deflate::tests` (feed pattern and thread count), and the CLI's
  `export_is_byte_identical_across_processes`. Anything new in the path
  (a filter heuristic, a chunk size, an encoder option) must be a function
  of the input only.
- **New lossy behaviour adds a `Compromise` variant** and is reported; it is
  never silent. Taxonomy today: `AlphaFlattened { onto }`,
  `NotRendered { what, count }` (walker shortfalls: text with no font,
  text on a path drawn on a straight baseline, quick shapes, images, live
  effects, unsupported clips), `FontSubstituted`,
  `WidenedFrom8Bit` (16-bit PNG from the 8-bit render),
  `ColourConverted { model, count }` (CMYK/spot → sRGB, every format),
  `ProfileDropped { images }` (raster and PDF), and for vector
  formats `Rasterised { node, reason, dpi }`, `Approximated { node, what }`
  and `BlendModeApproximated { node, ours, theirs }`.
- The export band height must stay a function of `(width, height)` only, and
  strips must start on the band grid.
- Nothing in `xarast-io` writes `.xar`.
- **SVG export has no mapper of its own**: it calls
  `xarast_format::svg::write_svg` with `SvgDialect::Interchange`, and an
  interchange file never contains `xarast:` (tests in `xarast-io`,
  `xarast-format` and the CLI corpus test check it). Sidecar image names
  are a function of document order only.
- **PDF: only `pdf/writer.rs` names `pdf-writer`.** Everything else speaks
  `Canvas`/`Resource`/`GState`.
- **PDF text: every font used is embedded and subset**, with `ToUnicode`,
  or the text is outlines and the report says `FontNotEmbedded`. Never
  a font reference without its program.
- **PDF numbers never carry `NaN`** (`writer::num` writes zero); geometry
  is passed in page points or path-local document units, never absolute
  millipoints, so the `f32` the file holds keeps 0.001 pt.
- **A soft mask's graphics state is set in page space** (before any `cm`):
  the mask form is interpreted in the coordinate system current at `gs`.
- **Rasterised pixels come from the raster exporter's code path**: a display
  list built from the same scene, commands matched by scene op index,
  `ListRasteriser` on `CpuConfig::deterministic()`.
- **Every rasterised or approximated object is reported**, per object, with
  its scene node (the document tag).
- **The built-in fixtures stay text-free** (`xarast-cli fixtures`): CI
  compares their bytes across two machines, and text would make them
  depend on installed fonts.
- **A new export-check limit needs its reason** in the limits file; the
  default stays 4/255.
- **PNG never carries both `sRGB` and `iCCP`** (`with_icc_profile` removes
  the former).

- **A derived filter's region comes from what the file says.** Never
  from cached bounds or the inherited line width of an unstroked
  element: those differ between an imported document and the same one
  read back, and a re-save stops being byte-identical (caught by
  `svg_roundtrip` on Groucho2 and feathers, 0.5 pt off).

## Dead ends (do not retry)

- **A feather as one `feMorphology`**: it erodes by a rectangle, √2 r
  deep on a diagonal edge, so stars and curves fade too early (feathers
  svg 5.06, Chrome 4.27). The four-rectangle union with `darken` is the
  fix (2.00 / 1.63).

- **`png` crate for interlaced output**: sets the IHDR interlace bit over
  non-interlaced data, producing corrupt files.
- **Optimised Huffman tables in JPEG** (`set_optimized_huffman_tables(true)`):
  `zune-jpeg` 0.5 — our own importer's decoder — misdecodes the 4:2:2 and
  4:2:0 files (libjpeg, Pillow and ImageMagick read them fine). Standard
  tables cost ~5 % in size. XARA-T-0193.
- **`image`'s built-in JPEG encoder**: fixed subsampling, no progressive.
- **Lossy WebP**: every usable encoder binds libwebp (C); none is pure Rust.
  Revisit only as a packaging decision (phase document risk table).
- **`oxipng`'s `timeout` for cancellation**: output would depend on timing.
- **`krilla` for PDF** (0.8.2): see the spike table — no sampled ramps, no
  meshes, `f32` `tiny-skia` geometry, a second `skrifa`, MSRV 1.92.
- ~~Rasterising an object over a transparent surface and un-premultiplying~~:
  a dead end only while the renderer darkened partly transparent Mix over
  nothing. Fixed by XARA-T-0231; it is valid now.
- **Rasterising a blend's backdrop over a transparent page**: the image
  composites a second time over the vector content under it. Render the
  backdrop over the paper.
- **`StreamShadingType` import**: `pdf-writer` 0.15 does not re-export it;
  the `/ShadingType 4` key is written by hand.
- **A second SVG writer for export** (walking the display list as PDF
  does): the phase forbids the fork, and the scene has already lost what
  SVG can say natively (groups, gradients as gradients, text as text).
- **Percent-encoding sidecar paths**: resvg does not decode them; sanitise
  the folder name instead.
- **Judging PDF on Poppler alone**: it strokes sub-pixel widths as a full
  pixel and mis-strokes `Broken Butt Cap`'s degenerate cap (12.6/255 where
  Ghostscript gives 0.97). `export-check` renders with both and takes the
  better; a read failure in either still fails.
- **Stripping the TrueType hinting tables to bring PDF text closer to
  our antialiasing**: SimpleText at 72 dpi Poppler 15.00 → 15.00, gs
  13.25 → 12.19. Poppler does not hint; the gap is its glyph rasteriser.
  Not worth the worse text in viewers that do hint.
- **`ttf2woff2` for WOFF2**: refuses CFF (`OTTO`) fonts (see text.md).
- **`pdftoppm -r 72` for a size-exact comparison**: it rounds the page up
  (277 × 181 for a 276.25 pt page); use `-scale-to-x/-scale-to-y`.
- **Comparing Poppler renders of placed images pixel for pixel**: Poppler
  resamples even at 1:1 (mean |Δ| 5–8/255 on the image cases); assert the
  report instead.

## Open TODOs

- ~~XARA-T-0192~~ (done, "File › Export"); XARA-T-0350 added the missing
  dialog options and the per-document hints. Still open: an export
  preview, the live portal round trip (Export… → chooser → file) on a
  compositor, PNG `--optimise` in the dialog, a Draft/Final choice, and
  the export hints for documents that are never saved after exporting
  (they are lost on close, by design: exporting is not an edit).
- XARA-T-0038 remainder: the per-pixel cost of gradient fills (~30 ns/px)
  and the on-screen/headless path's three bands; export itself is done.
- ~~T11.1.5 export hints in `meta.xml`~~ (done, XARA-T-0350), T11.1.6 batch export, T11.2.8 palette quantisation
  (PNG `Palette` is exact-only and refuses > N colours), T11.2.7 AVIF.
- Colour follow-ups: XARA-T-0249 (PDF `DeviceCMYK` option, our own sRGB
  profile for the output intent and WebP, `icc-color()`/`DeviceN` once a
  profile exists); colour-managing ICC images is phase 15. XARA-T-0248:
  compare our corpus renders with the previews the original embedded in
  each `.xar` (render TODO 5; the original itself cannot run here).
- `qpdf --check` has never run on this machine (not installed): the first
  CI `export` job is its first run over our PDFs.
- SVG follow-ups: ~~XARA-T-0233 (fonts: WOFF2 subset or outlines)~~
  (done, "Fonts"), XARA-T-0234 (precision), XARA-T-0235 (`Reference` resources, physical
  size, full minify); XARA-T-0236 (resvg check and comparison in CI) is
  done by XARA-US-0060; the
  shared bake ladder is XARA-T-0102 (profile).
- PDF follow-ups: XARA-T-0227 (bitmap transparency, ramp alpha, layer
  masks, per-family ΔE), ~~XARA-T-0228 (embedded subset fonts)~~ (done,
  "Fonts"), XARA-T-0229
  (images: DCT passthrough), XARA-T-0230 (multi-page, XMP, output intent;
  its `qpdf`-in-CI part is done by XARA-US-0060), XARA-T-0232 (ladder cost and file size). XARA-T-0231 is
  done: the raster exporters now honour `cap_end` and the dash offset as
  PDF does, so PDF and raster agree on both.
- A 20 000 × 20 000 WebP or JPEG holds the whole image (JPEG ≤ 65 535 px a
  side, WebP ≤ 16 383); only PNG streams. `MAX_EXPORT_PIXELS` is 2²⁹.
