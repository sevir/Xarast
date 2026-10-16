# text

Memory note for **text**: fonts, shaping, layout, glyph outlines, and the
contract with the document model. Phase 9 (`docs/phases/phase-09-text.md`).

## Current state

Round 1 of phase 9 (2026-09-23) built `xarast-text` in isolation. Round 2
(the same day, text integration) made **imported text render**: the text
model in `xarast-doc` (W9.2, read side), the attribute bridge and the
walker's text path in `xarast-app`, font service and substitution reporting.
`text_pending` is 0 on the whole corpus. Text on a path (W9.5,
2026-09-24) follows its path: `Designs/TextCurve.xar` draws every story
along its curve, `text_on_path_pending` is 0 on the corpus, and
architecture open question 4 is closed (see "Text on a path" below).

| Story | Tasks | State |
|---|---|---|
| XARA-US-0044 W9.1 font database | T9.1.1–T9.1.4 done; T9.1.5 (substitution ladder) implemented and tested too | in review |
| XARA-US-0046 W9.3 shaping and layout | T9.3.1–T9.3.4 done; T9.3.5 (line metrics), T9.3.6 (tracking, manual kerns, auto-kern), T9.3.7 (baseline, script, aspect) and a first T9.3.9 (bidi) came along because layout cannot return lines without them | in review |
| XARA-US-0049 W9.6 outlines | T9.6.1–T9.6.2 done; T9.6.3 convert command (XARA-T-0243) and T9.6.4 source text (XARA-T-0244) done, see "Convert to shapes" below; T9.6.5 export fallback done for PDF and SVG (XARA-T-0245, see "Font embedding"), profile C open | in review |
| XARA-US-0045 W9.2 text model | read side done: `StoryText`, `TextPos`/`TextCursor`, attribute bridge, importer scoping and surrogates; edit commands (T9.2.4): `InsertText`, `DeleteRange` done (XARA-T-0223), `SetTextAttr` done (XARA-T-0225), `InsertKern`, `SetStoryMode` open; story invariants (T9.2.5) open | in review |
| XARA-US-0047 W9.4 text tool | T9.4.1 state machine, T9.4.2 caret (blinking, split at direction boundaries), T9.4.3 selection spans in visual order, T9.4.4 keyboard navigation done; the T9.4.5 mouse gestures (click-to-position, drag, double/triple click) came along. T9.4.6 typing, grapheme deletion, undo per burst done (XARA-T-0223, with the T9.2.4 `InsertText`/`DeleteRange` commands). Text infobar, OpenType panel and interactive ruler (T9.4.9–T9.4.10) done (XARA-T-0225). IME composition and the text clipboard (T9.4.7–T9.4.8) done headlessly (XARA-T-0224, see "IME composition and the text clipboard"); the per-compositor check of the candidate window is open | in review |
| XARA-US-0048 W9.5 text on a path | T9.5.1 spike A, T9.5.2 spike B (test only), T9.5.3 measurement, T9.5.4 A shipped; the walker paints the followed path; convert to shapes follows the path (XARA-T-0246); T9.5.6 the base SVG follows the path (XARA-T-0252, per-character `rotate`, not `<textPath>`: see "Base SVG along the path"); T9.5.5 editing done (XARA-T-0251: fit, remove, reverse, indent handles, the shape editor on the path; word wrap on a path a documented non-goal, see "Editing text on a path") | in review |
| XARA-US-0050 W9.7 corpus | text renders in the walker (every corpus story); the `.xarast` text writer is exact (XARA-T-0172, done: 59/59 render round trip, T9.7.3); T9.7.1 tag inventory and T9.7.2 golden renders done (XARA-T-0260, see "TextDesigns acceptance gate"); T9.7.4 WOFF2/fsType done (XARA-T-0218, see "Font embedding"), T9.7.5 multi-script open (XARA-T-0261), gate against the original's reference bitmaps open (XARA-T-0262) | in review |

Public API (`crates/xarast-text/src/lib.rs`):

- **`FontDb`** (`font/mod.rs`) — `&self` everywhere, state behind one mutex,
  `Arc`-shareable. `new_system()` (enumeration deferred) /
  `load_system_fonts()`, `new_isolated()` (no system fonts; tests and golden
  renders), `with_options()`. `families()`, `query()` /
  `query_with_panose()` → `FontMatch { face, family, substitution, embedded,
  synthesis }`, `fallback_for(c, base)`, `set_fallback_preference(script,
  families)`, `set_generic_families()`, `register_embedded(name, bytes)`,
  `overlay()` + `register_document_face(family, bytes)` +
  `is_document_face()` (a document's faces, display only: "Embedded
  fonts on read"),
  `face_data()` → `FaceData` (shared blob + face index), `face_info()`,
  `embedding_denied()` (fsType), `substitutions()`, `glyph_outline()`,
  `glyph_outline_normalized()`, `normalized_coords()`, `units_per_em()`.
  `FaceId` is a dense `u32` index, stable for the database's life.
- **`Shaper`** (`shape.rs`, `layout.rs`) — `Shaper::new(Arc<FontDb>)`,
  `layout(&StoryInput) -> Layout`. Implements **`FontMetrics`**
  (`char_metrics`, `kern_pair`), the `FormatRegion` replacement.
- Input types (`style.rs`): `StoryInput { text, runs: &[StyleRange],
  paragraphs: &[ParagraphStyle], kerns: &[ManualKern], mode: StoryMode }`,
  `FontQuery`, `FontFeature`, `FontVariation`, `TextScript`, `Justification`,
  `LineSpacing`, `TabStop`/`TabKind`, `Direction`.
- Output (`layout.rs`): `Layout { lines, bounds, substitutions }`,
  `LaidLine { logical_range, paragraph, ends_paragraph, baseline_y,
  descent_line, ascent, descent, size, x, width, runs: Vec<GlyphRun>,
  clusters: Vec<LaidCluster> }`, `GlyphRun { face, size, aspect, coords,
  style, level, glyphs }` with `glyph_transform(g, upem) -> kurbo::Affine`,
  `PlacedGlyph { id, x, y, advance, cluster }`.

## Decisions taken (and why)

- **Dependencies.** `parley 0.11.1` + `fontique 0.11.1` (Apache-2.0 OR MIT),
  `skrifa 0.44` (MIT OR Apache-2.0; the version parley 0.11.1 links, not the
  0.47 of `research/05` — one skrifa in the graph, shared with `vello`),
  `unicode-bidi 0.3.18` (MIT OR Apache-2.0, already in the graph),
  `icu_segmenter`/`icu_properties 2.3` (Unicode-3.0, already allowed).
  `harfrust 0.12` arrives through parley. **No `rustybuzz`/`ttf-parser`**
  (RUSTSEC-2026-0192/0206). No `deny.toml` exception was needed.
- **`fontconfig-dlopen`** on fontique, like winit's `wayland-dlopen`: the
  build needs no fontconfig headers and the AppImage uses the host's
  libfontconfig, which it must (a bundled one cannot see the user's fonts).
- **The parley font context lives inside `FontDb`'s lock**, and the shaper
  locks the database while it shapes. So shaping sees exactly the faces the
  database knows (embedded ones included) with no collection clones to keep
  in sync. Cost: one `FontDb` serialises shaping across threads. Parallel
  layout later wants one database per worker or a shared fontique
  collection (`CollectionOptions::shared`), not a bigger lock.
- **Shape at a nominal 1000 units per em, convert once per glyph,
  accumulate in millipoints** (phase doc W9.3). Every run is shaped at
  `NOMINAL_SIZE = 1000.0` regardless of its size; an `f32` advance `v`
  becomes `round_half_away_from_zero(v × em / 1000)` mp, where `em` is the
  run's size × aspect (vertical offsets use the size). Guarded by
  `positions_accumulate_in_millipoints_without_drift`: a 500-character
  line ends at exactly 500 × one rounded advance, at 10 pt and at 9.973 pt.
- **We do our own line breaking, justification and line placement, not
  parley's.** Parley breaks and aligns in `f32` with CSS rules. Xara's rules
  (below) differ: width excluding the last character's tracking, spaces
  hanging, full-justification slack split between spaces and letters. So
  parley is asked for one unbroken line per paragraph and we use its
  clusters. Break opportunities come from ICU4X `LineSegmenter` directly
  (dictionary mode with the default `complex-scripts` feature): parley's
  `Cluster::is_word_boundary()` conflates word and line boundaries.
- **Bidi levels from `unicode-bidi`, not parley.** Parley resolves bidi but
  does not expose levels (only `is_rtl()` per run), and per-line visual
  reordering after our own line breaking needs them (UAX #9 L1-L2,
  `ParagraphBidiInfo::visual_runs`). Parley always auto-detects the base
  direction, so an explicit `Direction::Ltr/Rtl` is forced into it by
  prefixing U+200E/U+200F to the shaped text (and subtracting its length
  from every offset). Both sides therefore agree on the levels.
- **A cluster is never split.** Parley reports a ligature or a base with its
  marks as one "ligature start" cluster holding the glyphs plus
  "continuation" clusters with no glyphs and an equal share of the advance.
  `clusters()` walks text order, so continuations **follow** their start in
  an LTR run and **precede** it in an RTL run (lam-alef: lam is the
  continuation). We fold each group into one `ShapedCluster`, summing the
  `f32` advances and converting once.
- **Grapheme clusters are capped at 64 characters.** Parley 0.11.1 counts a
  cluster's characters in a `u8` (`src/shape/mod.rs:270`); "a" + 2 000
  combining marks overflowed it (a panic with overflow checks, silent wrap
  in release). `guard_long_graphemes` overwrites every character past the
  64th of a grapheme with U+0001 filler of the same byte length (offsets
  stay valid) and the filler's clusters are folded into the grapheme's head
  with no glyphs. Real text never gets close (stream-safe text allows 30
  non-starters). Worth reporting upstream.
- **Manual kerns and tracking are thousandths of an em**, not millipoints —
  see the facts below. `StyleRange::tracking` is `i32` em/1000 and
  `ManualKern::amount` too; both are converted with the em width of the
  style in force (`em = effective size × aspect`, via `Mp::mul_ratio`,
  rounding half away from zero).
- **Full-justification remainder is distributed**, 1 mp at a time to the
  first recipients, so a justified line is exactly its column's width
  (acceptance criterion 6 then holds to 0 mp). The original truncates the
  per-gap share and drops the remainder; the difference is < 1 mp per gap.
- **The substitution ladder** (T9.1.5) as implemented in
  `font/substitute.rs`: (1) exact name, case- and whitespace-insensitive
  (fontique's own lookup is case-insensitive; we collapse whitespace);
  (2) trailing style words stripped (`thin … black`, `italic`, `oblique`…;
  **not** `roman`, which ends "Times New Roman"), their weight/italic applied
  to the query; (3) metric-compatible aliases; (4) the generic family, from
  PANOSE when the document has it (digit 1 = 2 Latin text; digit 4 = 9
  monospaced; digit 2 = 11–13 sans, 2–10/14–15 serif) else from the name;
  (5) `system-ui`, then `sans-serif`, then the first family in sorted order.
  Every substitution is recorded (`FontDb::substitutions`,
  `Layout::substitutions`) and never written back to the document.
- **The alias table's contents and provenance**: groups {Arial, Helvetica,
  Liberation Sans, Arimo, Nimbus Sans, Nimbus Sans L, TeX Gyre Heros,
  FreeSans}, {Arial Narrow, Liberation Sans Narrow, Nimbus Sans Narrow},
  {Times New Roman, Times, Liberation Serif, Tinos, Nimbus Roman, Nimbus Roman
  No9 L, TeX Gyre Termes, FreeSerif}, {Courier New, Courier, Liberation Mono,
  Cousine, Nimbus Mono PS, Nimbus Mono L, TeX Gyre Cursor, FreeMono},
  {Calibri, Carlito}, {Cambria, Caladea}, {Georgia, Gelasio}. Written by us
  from the public fact that these families were published as
  metric-compatible replacements; not transcribed from Xara or from any
  fontconfig configuration.
- **Embedded faces shadow system faces.** `register_embedded` registers into
  fontique's own ("ours") family map, which `family_id()` consults before
  the system map. So once a document registers "Noto Sans", a query for any
  Noto Sans weight gets the embedded face (synthesised bold if need be),
  never the installed family. Asserted against real system fonts in the
  opt-in test.
- **Embedding denied** when the licence level of `OS/2.fsType` is
  *restricted* (bit 1 without bits 2–3) or bit 9 (bitmap embedding only)
  is set; unreadable tables count as installable, the OpenType default.
  The rule lives in `embed::EmbedRights::from_fs_type` (see "Font
  embedding"); `FontDb::embedding_denied` is its shorthand.
- **Glyph outlines** are drawn unhinted at unit scale into `BezPath` and
  cached per `(face, glyph, coords)` with trailing zero coordinates
  stripped, so `[]` and `[0]` hit the same entry. The cache is cleared
  wholesale at 65 536 entries rather than evicted. Drawing at a size is
  `GlyphRun::glyph_transform` (scale `size/upem`, × aspect horizontally,
  then translate), never a re-extraction.
- **Coordinates are y up**, like the document: the first baseline is y = 0,
  later ones negative; x = 0 is the column's left edge (column mode) or the
  anchor (point mode). Glyph y offsets from parley (y down) are negated.

## Font embedding (XARA-T-0218, XARA-T-0228, XARA-T-0233, XARA-T-0245, as built)

One layer, `xarast-text/src/embed/` (`embed::{EmbedRights, Embedding,
EmbedError, PdfFont, WebFont, woff2, with_fs_type}`), used by every
writer: `.xarast` and SVG export (WOFF2 behind `@font-face`) and PDF
export (subset `CIDFont`s). Nothing else decides licences or subsets.

- **The `fsType` rule** (`EmbedRights::from_fs_type`): level = bit 9 →
  `BitmapOnly` (refused: we embed outlines); else the least restrictive of
  bits 3 (`Editable`), 2 (`PreviewPrint`), 1 (`Restricted`, refused); 0 →
  `Installable`. Bit 8 = no subsetting → the **whole glyph set** is
  embedded (`PdfFont::whole`, `WebFont::whole`; the web font's `cmap`
  then maps every character the face maps). `PreviewPrint` *is* embedded
  in `.xarast` too: Xarast only draws an embedded subset for display and
  never installs it for editing (the reader registers it for display
  only: "Embedded fonts on read" below).
- **Subsetter: `subsetter` 0.2.6** (Typst, `MIT OR Apache-2.0`, default
  features off so no second `skrifa`/`write-fonts`). It keeps outlines,
  `head`/`hhea`/`hmtx`/`maxp`/`name`/`post` (+ `cvt`/`fpgm`/`prep` for
  TrueType) and converts CFF to CID-keyed CFF with identity CIDs; it drops
  `cmap` and `OS/2` because a PDF CID font brings its own maps. Glyphs are
  handed to it sorted with `.notdef` first, so **new gid = index in the
  kept list**; composite components it adds go after them. Panics inside
  it are caught (`xarast_unwind::expect_panics`, so the app's panic hook
  writes no crash report) and become `EmbedError::Subset`: the
  input is a system font, not ours to trust.
- **PDF program** (`FontDb::pdf_font`): TrueType → the whole subset file
  (`FontFile2`, `CIDFontType2`, `CIDToGIDMap /Identity`); CFF → the bare
  `CFF ` table (`FontFile3 /CIDFontType0C`, `CIDFontType0`). Widths,
  bbox, ascent/descent/cap height and italic angle from `skrifa`
  metrics, in thousandths of an em; PostScript name from `name` ID 6.
- **Web font** (`FontDb::web_font`): subset of the glyphs the drawn
  characters map to, plus a **`cmap` we build** (`embed/cmap.rs`: format
  4 for the BMP, format 12 added when a character is above U+FFFF; the
  format-4 terminator maps U+FFFF to glyph 0 — a delta of 1, got wrong
  once) and the face's **own `OS/2` unchanged** (browsers require it; its
  `fsType` still states the licence). Then **our own WOFF2 writer**
  (`embed/woff2.rs` + `embed/woff2_glyf.rs`, from the W3C spec): the
  **`glyf`/`loca` transform** (§5.1–5.3, version 0; XARA-T-0276) for
  TrueType faces, every other table with the null transform, `loca`
  right after `glyf`, one Brotli stream (`brotli` 9, quality 11, font
  mode, single-threaded, so deterministic). A `glyf` that does not parse
  falls back to the null transform (version 3); CFF faces have none.
  The transform packs each point as a triplet (the shortest of the
  spec's 128 classes), stores a box only when it is not the points' own
  (always for composites), keeps instructions and the overlap bit, and
  gains 14 % on the pinned Latin subset (26 516 → 22 728 bytes), 7 % on
  the Arabic one; a synthetic 5-glyph face grows by 8 bytes (kept anyway:
  real subsets always gained). `woff2::decode` reads any single-font
  WOFF2: null transforms, the `glyf`/`loca` transform (its own `glyf`
  flag packing, padding 2 or 4 by `indexFormat`, coordinates checked to
  `i16`) and the `hmtx` transform (§5.4, dropped bearings = the glyph's
  `xMin`); collections are refused. It is checked against **another
  writer's file** (`tests/fonts/NotoSans-Bold.subset.ttf2woff2.woff2`,
  made once by `ttf2woff2` 0.13.3 outside the workspace: every other
  table byte-identical, every outline, advance and box equal) and
  against byte flips and truncations of a transformed `glyf` (no panic).
  Chrome loads the null-transform files (a probe with a family name no
  system has renders the embedded glyphs: hebrew mean |Δ| 0.85 vs our
  PNG; a corrupted `data:` URI changes the render by 21/255); the
  probe has **not** been re-run on transformed files (XARA-T-0276
  report).
- **Why not `ttf2woff2`** (MIT OR Apache-2.0, applies the `glyf`
  transform): it refuses `OTTO` (CFF) fonts, and the pinned CJK face is
  CFF; it has no decoder either. It only made the decoder's fixture. **Why not `fontcull`/`klippa`**: MIT-only, another
  `read-fonts`/`write-fonts` line, C-bound WOFF2 (`woofwoof`).
- **Cost** (release, this machine): Noto Sans subset, 95 characters →
  6.3 KB WOFF2 in 7.5 ms (Brotli q11 is nearly all of it); CJK 14
  characters → 2.3 KB in 3.6 ms; DejaVu Sans (760 KB face) 95
  characters → 15 KB in 36 ms; a PDF program is microseconds.
  `FontService::web_font` caches results by `(face, sorted chars)` (64
  entries, cleared when full), so autosave and repeated saves of an
  unchanged document subset nothing.
- **Where the glyphs come from.** `.xarast`/SVG: `SvgTextPlacer::place`
  (the walker's layout) reports `StoryPlacement::faces` (each face with
  the characters it draws), `char_faces` (face of each character item)
  and `denied` (requested families whose own face refuses embedding).
  PDF: `build_story` keeps, per run, `ExportGlyph { face, id, transform
  (font units → document), variable, text }` — `text` is the cluster's
  text on its first glyph — and the underline bars; the walker records
  the painted runs (`SceneWalker::scene_text`).
- **Variable faces** are subset at their default instance (no
  `variable-fonts` feature): a run drawn at another instance is not
  *painted* as text in PDF (outlines + invisible text); in SVG the
  browser draws the default instance.

Evidence: `xarast-text/tests/embed.rs` (5: outlines of every drawn
character identical after WOFF2 decode, Latin/CFF CJK/Hebrew; PDF widths
and CFF program; refusal for fsType 2 and 0x200, preview & print allowed;
no-subsetting embeds all glyphs; empty sets refused; since XARA-T-0276
also another writer's transformed WOFF2 decodes to the same font, and
our web fonts are `glyf`-transformed) + unit tests (`sfnt`, `woff2`,
`woff2_glyf`: triplet classes, `255UInt16`, `hmtx`, corruption, a
second pass is stable; `fsType` table); `xarast-app/tests/font_embedding.rs`
(4, end to end on a synthetic document, the corpus `embeddedFonts.xar`
has no font data: PDF text/clip/invisible modes, one `FontFile2`,
`ToUnicode`, `pdftotext` and `pdffonts` read it back, the refused face
never embedded and reported; SVG export embeds one `@font-face` and
outlines the refused story; `--text outlines`; `.xarast` has exactly one
`resources/fonts/*.woff2`, `xarast:font-embed="denied"`, `xarast:fonts`
in `meta.xml`, and re-saves byte-identical fresh and through raw copies).

## Embedded fonts on read (XARA-T-0276, as built)

A `.xarast` carries a WOFF2 subset per face its text draws with
(`resources/fonts/`, one `@font-face` each). The format reader keeps the
files in `DocumentResources::fonts()` (`EmbeddedFont { family, weight,
italic, data }`, `docs/memory/xarast-format.md`); the application lays
the document out with them where the machine lacks the face.

- **Per-document overlay, not the process database.**
  `FontDb::register_embedded` shadows a family for everyone, so it is
  not used. `FontDb::overlay()` makes a new database over a *clone* of
  the base database's `fontique` collection (the system data is behind
  `Arc`s: no second enumeration; registered faces, generic families and
  fallback preferences come along) with its own face table, LRU and
  source cache — face ids are the overlay's own. `FontService`s over
  overlays come from `fonts::for_document(base, doc)`: `base` itself when
  the document embeds nothing (or `base` already is its overlay),
  otherwise an overlay with every file decoded (`woff2::decode`, or used
  as is when it is an OpenType file) and registered. Eight are
  remembered, keyed by `Arc::ptr_eq` of the base and of each file's
  bytes (so a document and its clones share one; the cache holds the
  `Arc`s, so no pointer is ever reused). Two documents embedding
  different faces under one family get two overlays: neither sees the
  other's face, and the base sees neither (test
  `documents_embedding_different_faces_of_one_family_do_not_interfere`).
- **Private family.** `register_document_face(family, bytes)` registers
  the face under `U+F8FF` + the lower-case normalised family, a name no
  real family has: it never shadows the machine's family, `families()`
  filters it out (the font list never offers a subset for editing), and
  no fallback chain names it. `FaceInfo::family` is the real name, so
  the writer's `@font-face` and substitution reports read normally.
  `doc_faces` maps each handed-out document face to its private family:
  `DbInner::parley_family` is what the shaper asks `parley` for.
- **Priority rule** (`DbInner::match_family`, used by every rung of the
  ladder): a document face of family F is used only when the machine has
  **no face of F with the same weight, style and width** as the document
  face that best matches the query; otherwise the machine's match wins,
  even when the document has a face too. Why: the machine's face has
  every glyph (the subset has only the characters drawn at save time)
  and its `GSUB`/`GPOS` (the web font has none, so a document face loses
  kerning, ligatures and joining); where the machine has that exact face
  the layout is the writer's own. Where it lacks it, the document face
  beats a synthesis: the writer drew Bold, a reader with only Regular
  would embolden it, the embedded Bold is the real thing. Comparing
  attributes, not versions: a different version of the same face on the
  reader still wins (it is complete). A variable machine face matches
  on its default instance.
- **Typing outside the subset.** The shaper's family chain for a
  document face is `[private family, the machine's F (when it exists),
  sans-serif]`; `parley` picks per cluster, so a character the subset
  lacks comes from the machine's face of F, then the usual fallback —
  never `.notdef` (test `typing_outside_the_subset_uses_a_real_face`:
  `Q`, `z`, `!` from real faces, `bold` from the subset, no glyph 0).
- **Who uses it.** The walker keeps its base service (`with_fonts` or
  the shared one) and lays stories out with `story_fonts(doc)` =
  `for_document(base, doc)`, dropped with the story cache when
  `fonts::serves` says it no longer fits the document (another document,
  other files); `scene_text` hands exporters the overlay's database
  (the glyphs' face ids are its). Also: the text tool (`doc_fonts`),
  picking, convert to shapes (command and `EditCommand`), the fit of
  `headless::render_with_fonts` and of `drawing_rect` without explicit
  fonts, `xarast-cli` export's outline fallback, and the `.xarast`/SVG
  text placer (`SvgTextPlacer` places each document with its overlay and
  remembers `(service, face)` per `PlacedFace::key`; keys of a single
  service are the face indexes as before, so re-saves stay
  byte-identical). A re-save on a machine without the face therefore
  embeds the subset of the document's subset again.
- **Evidence** (`xarast-app/tests/embedded_fonts_read.rs`, pinned fonts
  with and without `NotoSans-Bold` in a `from_dir` directory, which is
  what `XARAST_FONT_DIR` builds): a bold + regular document saved with
  the full set, reopened, renders **pixel-identical** with the bold face
  removed; the same document without its embedded faces renders
  differently there (synthetic bold); the overlay uses the document's
  bold only on the machine without it, never the regular; `families()`
  is unchanged; the re-save carries both faces again and renders the
  same. The corpus round trip (59/59) and TextDesigns digests are
  unchanged: with the pinned fonts every embedded face exists on the
  machine, so the rule picks the machine's.
- **Dead end: registering into the shared service** (what
  `register_embedded` does) — a document's subset would shadow the
  machine's family for every open document, and typing would find no
  glyphs.
- Open: `viewport::drawing_rect_with` with explicit `&FontService`
  cannot build an overlay (no `Arc`), so its callers pass one already
  (headless does); a document face is laid out without `GSUB`/`GPOS`.

## The text tool (W9.4, as built, XARA-US-0047)

Code: `xarast-app/src/text_edit.rs` (`CaretMap`, pure, story space),
`xarast-app/src/text_tool.rs` (`TextTool`, the state machine), `tools.md`
decisions 53–56 and `ui.md` decision 40 for the tool and shell sides.

- **A caret is a byte offset of the laid-out text plus an affinity**
  (`Caret { byte, upstream }`), not a `TextPos`: layout, hit testing and
  bidi all speak byte offsets, and a `TextPos` cannot say which side of a
  soft line break or a direction boundary it means. `TextSelection::cursor`
  derives the model's `TextCursor` (line, item) through
  `StoryText::pos_of` when something needs it. The laid-out text is
  `layout_text` (the story's last `'\n'` stripped), so the last caret
  offset is before the final EOL item.
- **Stops.** A line's caret stops are its clusters' two edges in x order:
  an LTR cluster's left edge is its start (downstream) and its right edge
  its end (upstream); an RTL cluster the other way round. Neighbouring
  clusters share an x: the same offset in one-direction text, two
  different offsets at a direction boundary. A cluster is never split, so
  a ligature or a base with its marks is one stop (T9.3's rule).
- **Visual movement** (the default; `TextTool::logical_arrows` is the
  preference) walks stops by x: right = the first stop further right,
  which is the trailing edge of the cluster crossed; left = the nearest
  stop further left. Off a line's end it goes to the logical start of the
  next line or the logical end of the previous one, by the line's base
  direction (`LaidLine::base_rtl`, added for this). Logical movement,
  word movement (UAX #29 via `xarast_text::word_segments`, dictionary
  data with `complex-scripts`), Home/End (logical line start/end: the right
  end of an RTL line is its start), Up/Down with a goal x kept across the
  run, Page Up/Down (lines per view height) and Ctrl+Home/End are the
  other motions. Up on the first line goes to the story start, Down on the
  last to its end (the macOS behaviour; Windows stays put — pick again if
  the maintainer prefers). Arrow keys pointing right in an RTL paragraph
  mean logical *backward* for logical and word motion.
- **Split caret.** The primary caret is the stop the caret sits on (its
  offset *and* side), so repeated arrows move it steadily across the
  screen; the secondary, drawn half height, is the other place the same
  offset is drawn at a direction boundary. Not Pango's strong/weak rule
  (strong = paragraph direction): with that one the drawn caret jumped
  back across the Hebrew run while the user pressed Right (dead end).
- **Soft line ends.** `line_of` shows an offset shared by two lines on the
  upper one when upstream, the lower one otherwise. End on a wrapped line
  gives an upstream caret at the line's end (trailing spaces included);
  Right from there moves to the start of the next line *at the same
  offset* (visual-only move) and Left comes back.
- **Hit test**: nearest line by its band (descent below to ascent above
  the baseline), then the nearest stop; between two stops at one x the
  one on the pointer's side wins.
- **Selection spans**: per line, the selected clusters' boxes merged left
  to right (so mixed text gives several spans, in visual order), plus a
  block a third of the line's size wide at the line's logical end when the
  paragraph break after it is selected.
- **Pending caret.** A click on empty canvas or a column drag makes a
  *pending* caret, not a story; the first typed character creates it
  (below).
- **Text on a path** (`OnPath`, XARA-T-0250): the tool lays the story out
  exactly as the walker does (`text::lay_story`: the path's column, then
  the fit) and builds `CaretMap::on_path(text, layout, fit)`. Motion is
  unchanged (straight layout); drawing and hit testing go through the
  fitted clusters — see "Carets on a path" under "Text on a path".

Measured (`cargo bench -p xarast-app --bench text_caret`, 10 000
characters, Latin + Hebrew, 300 pt column, pinned fonts; budget 1 ms per
motion): visual right 0.72 µs, logical right 2.1 µs (122 µs before the
move looked only at the caret's line and its neighbours), word right
1.1 µs, line down 1.2 µs, line end 0.40 µs, hit 0.55 µs, selection spans
of the whole story 54 µs.

Tests: `xarast-app/tests/text_caret.rs` (15: every assertion on byte
offsets — Latin, RTL, mixed walks, split caret, hit round trip, right n /
left n, selection spans, the paragraph-break block, soft line ends, goal
x, words, empty story, marks; plus the corpus walk over every line of
`hebrew.xar`: visual order strictly rightwards, every stop visited),
`tests/text_tool.rs` (10, through `Session` intents: document untouched),
`xarast-shell` `a_text_caret_takes_the_navigation_and_character_keys`,
`xarast-ui` `the_caret_blinks_from_on_then_stops_blinking`.

## Typing and deleting (T9.4.6, as built, XARA-T-0223)

Code: `xarast-doc/src/text_edit.rs` (`InsertText`, `DeleteRange`,
`insert_text`, `delete_range`, `new_story`), `xarast-app/src/ops.rs`
(`EditCommand::{TypeText, DeleteText, CreateText}`), `text_tool.rs`
(bursts), `xarast_text::{prev_grapheme, next_grapheme}`.

- **Offsets, not `TextPos`.** The edit commands take byte offsets into
  `StoryText::text` (the phase doc sketches `TextPos`): the tool, layout
  and hit testing speak offsets, and an offset survives the line split an
  edit makes where a `(line, item)` pair does not.
- **What an insertion does to the tree.** One `TextItem` node per
  character, right after the character before the offset, so new text
  takes that character's style; that character's own attribute children
  are copied onto each new item. After a paragraph break or at a line's
  start the items go before the first item at the offset instead.
  `'\r\n'`/`'\r'` → `'\n'`, `'\t'` → `Tab`, other controls dropped.
- **`'\n'` splits the line**: a `LineBreak(true)` is inserted and
  everything after it moves to a new `TextLine` right after, which starts
  with copies of the attributes the line had in scope at the split (last
  per slot) and the same ruler. No character changes style (asserted),
  and every line still ends with its break.
- **Deleting a paragraph break does not merge lines**: the line simply no
  longer ends with a break, and `paragraph_first_lines` runs the paragraph
  on into the next line (paragraph style from the first line). Merging
  would need explicit attributes to keep the moved characters' style;
  leaving the lines costs nothing (they are derived state; `format_story`,
  T9.3.12, will re-line). A line left with no items is removed unless it
  is the story's last. Kerns/soft breaks strictly inside a deleted range go
  with it; one at the range's start stays.
- **Undo record = the per-node actions**, proportional to the text typed
  or deleted. The phase doc's "store the paragraph's item run before and
  after" (and its 4 KiB diff threshold) is not needed while nothing
  reflows into the arena: there is no wrap restructuring to undo. Revisit
  with T9.3.12.
- **New story** (`new_story`): `TextStory` (matrix = translate to the
  pending caret, `AtPoint` or `InColumn { width, word_wrap: true }`) →
  the current attributes as its own children → one `TextLine` holding only
  `LineBreak(true)`, the final EOL every story ends with (so the caret's
  last offset is before it, as for imported stories). Then the text is
  inserted at 0.
- **Grapheme-aware deletion**: Backspace/Delete remove one extended
  grapheme cluster (ICU4X; a base with its marks, a ZWJ emoji sequence,
  CRLF), not one code point; Ctrl deletes to the logical word motion's
  target (Ctrl+Delete = up to the next word's start, as Ctrl+Right). A
  selection is deleted whole; typing replaces it. The final EOL is outside
  the laid-out text, so it can never be deleted from the tool.
- **Typing bursts** (`tools.md` decision 57): one undo step per burst,
  merged by `CoalesceKey { gesture: burst, kind: "text-typing" |
  "text-delete" }` in the history.

Tests: `xarast-doc` `text_edit::tests` (5: style inheritance, line split
keeps every style, joining paragraphs, bad offsets refused, new story),
`xarast-text` `graphemes_step_over_marks_and_emoji_sequences`,
`xarast-app/tests/text_tool.rs` (6 new: 200 characters = one undo step,
burst ends, graphemes, selection replace + Enter, pending → story in one
step, column wrap), `xarast-shell`
`a_text_caret_takes_the_navigation_and_character_keys` (typing via key
events).

## Removing an emptied story (XARA-T-0237, XARA-T-0295, as built)

Maintainer's decisions (2026-09-24): a story emptied by deletion is
removed, as the original does, **in the same undo step as the deletion**
(T-0237); and a story holding **no characters, only paragraph breaks**, is
removed too, **also when editing ends** (T-0295). Code:
`xarast-doc/src/text_edit.rs` (`holds_no_characters`, `is_story_empty`,
`remove_empty_story`, `EmptyStory`), `xarast-app/src/ops.rs`
(`remove_if_emptied`, called by `DeleteText`, `CutText`, and `TypeText`/
`PasteText` of nothing; `EditCommand::RemoveEmptyStory`), `text_tool.rs`
(`Emptying`, `leave`, `edited`, `last_burst`, `after_commands`); tool side:
`tools.md` decision 70 and invariant 11.

Facts about the original (read, not copied): the empty story is removed by
`OpDeleteTextStory` (`tools/textops.cpp:4266-4300`), which merges itself
into the previous operation (`PerformMergeProcessing`, `:4363-4366`) — so
undo brings back story and text together there too. It runs when editing
*ends*: Esc (`tools/texttool.cpp:1570`), deselecting the tool (`:2403`),
clicking into another story (`textops.cpp:3893`), creating a new one
(`:333`). "Empty" there means no `TextChar` (paragraph breaks and tabs do
not count; `:4321-4350`). A story on a path gives its path back: the
story's attributes are localised, the path moved next to the story and
deselected, then the story hidden (`:4281-4298`).

- **Empty = no characters** (`holds_no_characters`): `StoryText::text`
  holds only `'\n'` (paragraph breaks) and `'\t'` (tabs). **Tabs do not
  count**, as in the original (it searches for a `TextChar` and a tab is
  not one); soft line breaks and kerns are not in the text at all. A space
  is a character. (T-0237 first built "nothing but the final break";
  T-0295 widened it to the original's rule.)
- **When, 1: inside a deleting command.** `DeleteText`, `CutText`, and a
  `TypeText`/`PasteText` that inserts nothing, remove the story in their own
  transaction when they leave it without characters; that costs no extra
  step and merges with the Backspace burst (`CoalesceKey` unchanged). A
  deletion of nothing never removes. **Typing that inserts something**
  (Enter over a selection of all the text) never removes at the command,
  even when it leaves only breaks: the user is typing; the story goes when
  editing ends.
- **When, 2: when editing ends** (T-0295): Esc, a tool switch (the tool's
  `on_deactivate`), a click on the canvas or into another story — every
  path goes through `TextTool::set` changing the edited story, which calls
  `leave`. The tool emits `EditCommand::RemoveEmptyStory` when the story
  it leaves **was edited by the tool since the caret went into it**
  (`edited`: typing, deleting, cut, paste, an infobar/ruler edit applied
  to it, or the story typing/paste created), is still reachable and
  editable, and holds no characters. **An imported empty story that was
  only entered is never touched** (tested with `"\n"` and `"\n\n"`).
- **Merging on leave.** The original merges its story deletion into the
  previous operation, whatever it was. We merge only into the tool's own
  last burst on that story: `RemoveEmptyStory { joins }` carries the
  burst's `CoalesceKey` (`last_burst`, recorded in `after_commands`), and
  the history merges only when the last step still has that key. When it
  does not (the burst was undone, or a paste, cut or attribute edit came
  last), the removal is **its own "Delete Text" step**: merging into an
  unrelated step would make one undo take back more than the user did.
  Enter ×3 at a new caret then Esc: one step ("New Text"), one undo gives
  the digest from before the first Enter.
- **The caret afterwards**: pending at the story's origin (matrix
  translation; column width kept; rotation and shear not kept), carrying
  the removed text's attributes that differ from the current ones as the
  pending style (character, paragraph and the text's fill/line colour), so
  typing on rebuilds the same-looking story on the active layer.
- **On a path**: the path stays where the story was (z-order), a copy of
  the path node with, as its own attributes, the non-text attributes it
  painted with that differ from what it inherits (the story's included) —
  the same rule as convert to shapes; text attributes are dropped. Editing
  ends (no caret) and nothing is selected.
- **Selection**: the removed story is pruned from it; the freed path is
  not selected.
- **Damage**: removal is an ordinary node deletion; the walker's text ink
  goes with the scene, so the repaint is exact (asserted, below, for both
  the deletion and the leave paths).
- **History bug found on the way (fixed in `history.rs` `reap`).** A
  commit drops the redo branch *after* its own actions ran, and `reap`
  destroyed every retained node that was unreachable. A redo branch
  retains nodes it moves (the break an undone Enter moved), and a commit
  deleting their story makes them unreachable but still inside the
  subtree that commit's undo needs: one undo then gave back the story
  without its final line. `reap` now destroys only detached roots (no
  parent); a nested node goes with the root it hangs from. It affected any
  object deletion after undoing an Enter, not only this feature.

Tests: `xarast-doc` `text_edit::tests` (4: a deletion leaving only breaks
removes the story and one undo restores it; story on a path leaves a path
that paints the same; breaks and tabs are not characters; deleting a story
after undoing a break undoes to the whole story); `xarast-app/tests/
text_tool.rs` (T-0237: Select All + Delete removes the story in the one
"Delete Text" step, pending caret at the origin, undo digest exact; typing
on rebuilds it in place and style; a Backspace burst emptying a new story
is one step; on a path; the repaint equals a full render, and its undo the
first frame. T-0295: Enter ×3 then Esc removes the story merged into the
typing step, one undo gives the digest from before; a tool switch, a click
on the canvas and a click into another story remove a story of a tab and
breaks; after the burst was undone the removal is its own "Delete Text"
step; deleting every character but the breaks removes in that step; Enter
over all the text keeps the story until Esc; an imported story of breaks
survives being entered and left; the leave repaint equals a full render;
a story deleted after undoing a break undoes whole);
`tests/text_clipboard.rs` `cutting_all_the_text_removes_the_story_in_the_same_step`.

## IME composition and the text clipboard (T9.4.7–T9.4.8, as built, XARA-T-0224)

Code: `xarast-app/src/text_clip.rs` (`StyledText`, `copy_range`,
`paste_edits`, `TextClipOp`), `ops.rs` (`EditCommand::{PasteText,
CutText}`, `PasteTarget`), `text_tool.rs` (composition, cut/paste),
`text.rs` (`splice_text`), `walker.rs` (`paint_story` with a composition),
`app.rs` (`TextClipboard`), `xarast-shell` `viewer.rs` (`ime_request`,
`ime_event`). Shell and tool sides: `ui.md` decision 41, `tools.md`
decision 63.

- **A composition is a preview, never an edit.** `Intent::TextPreedit(
  Option<Preedit>)` → `Tool::text_preedit`; the text tool puts
  `Preview::text = TextPreview { story, at, text }` and the walker draws
  that story with the text spliced in (`splice_text` on the collected
  `StoryText`: the inserted text takes the style typing there would give
  it — the character before, or the one after at a paragraph's start —
  and every run, line start, item and kern after it moves along). The
  spliced geometry is never cached and its content hash folds the
  composition in. The tool lays out the same spliced story for the caret
  (at the composition's cursor end), a plain underline (the bottom edges of
  the composition's selection quads), and a highlight of the IME's
  selected segment; `cursor: None` hides the caret. The composition shows
  at the *start* of the selection, which its commit replaces.
- **A commit is typing**: `ImeEvent::Commit` → `Intent::TextInput(Insert)`,
  same burst rules, so a CJK run coalesces like typed Latin. Typing,
  a paste or any caret change drops the composition and its preview.
- **At a pending caret there is no story to draw it in**: the composition
  is not shown (only the candidate window follows the caret); the commit
  creates the story (XARA-T-0268).
- **Copy with attributes.** `copy_range` keeps the plain text and, per run,
  the values of `CHAR_SLOTS` (font, bold, italic, aspect, tracking,
  underline, size, script, baseline, features) plus `PAINT_SLOTS` (fill,
  line colour). Paragraph attributes are not carried: the text takes the
  paragraph it lands in. Colours are dropped when pasting into another
  document (`without_paint`): they may name the source palette.
- **Paste writes only differences.** `PasteText` inserts the plain text
  (so it takes the local style, as typing), re-collects the story and
  sets, per carried slot, only the ranges whose value differs
  (`paste_edits`): text slots through `set_text_attr`, colours as the
  items' own attributes (`fill_edit::set_own_attr`). Pasting into text of
  the same style adds no attribute node (asserted).
- **One step each**, labelled "Paste" and "Cut"; a paste at a pending
  caret creates the story in the same step (`PasteTarget::New`) and the
  caret moves into it (`TextTool::adopt`, the `CreateText` pattern).

Tests: `xarast-app/tests/text_clipboard.rs` (10: styled round trip and
one-step undo, same-style paste adds no nodes, foreign text plain with
CRLF normalised, cut, no copy/cut without a text selection, our object SVG
refused in text, paste at a pending caret, composition drawn/undrawn with
the walker's text ink, composition at a pending caret, a click ends the
display), `xarast-app` `text::splice_tests`, `text_clip::tests`,
`xarast-shell` `the_input_method_follows_the_text_caret_and_composes_in_the_story`
(synthetic `winit::event::Ime` through `translate_ime`) and
`ctrl_c_x_v_at_a_text_caret_move_text_not_objects` (a fake `Clipboard`).

## Text attributes: `SetTextAttr`, the infobar, the ruler (XARA-T-0225, as built)

Code: `xarast-doc/src/text_edit.rs` (`SetTextAttr`, `set_text_attr`,
`is_paragraph_slot`, `paragraph_line_range`, `text_attr_label`),
`xarast-app/src/text_infobar.rs` (values shown, field → attribute, feature
and tab edits), `text_tool.rs` (targets, pending style),
`ops.rs` (`EditCommand::SetTextAttr { story, edits, burst }`),
`xarast-ui/src/toolbar.rs` (widgets) and `text_ruler.rs` (the ruler).

- **Byte ranges, like the other edit commands.** `SetTextAttr { story,
  range, value }`; the app's `EditCommand::SetTextAttr` carries several
  `(range, value)` pairs so one step can set, per run, a feature list that
  keeps each run's other features.
- **A character attribute becomes an own attribute child of every item in
  the range** (chars, tabs, paragraph breaks; not kerns), replacing the
  item's own child of that slot (`set_attr` when one exists). Own children
  apply to their item alone (the collector pushes them in the item's own
  scope), so nothing outside the range changes, and `insert_text` copies the
  previous character's own children onto typed text, so typing after bold
  text is bold. No sibling attribute + "restore" pair (the importer's
  representation): it would need the value in force after the range, per
  slot, and a delete could strand the restore.
- **A paragraph attribute** (justification, line spacing, left/right margin,
  first-line indent, ruler: `is_paragraph_slot`) goes on **every line of
  every paragraph the range touches** (`paragraph_line_range`; the caret's
  paragraph for an empty range) as one line-level attribute right before the
  line's first item; other attributes of that slot before the first item,
  and the first item's own child of that slot, are removed, because layout
  reads a line's attributes at its first item. A line with no items is left
  alone (it reads its attributes from outside itself).
- **Setting a value an item already has** changes nothing but still records
  an (empty) step through the bare bus; the tool therefore never emits an
  edit whose value is already shown (`plain_edit`/`story_edits` compare).
- **OpenType features** are Xarast's own attribute slot `TxtFeatures`
  (`AttrValue::FontFeatures`, `FeatureSetting { tag, value }` sorted by tag,
  `document-model.md`), bridged to `StyleRange::features` and written as
  `xarast:features` (`research/06 §6.7`). The panel offers liga, dlig, smcp,
  c2sc, onum, lnum, tnum, pnum, frac, zero, swsh, ss01; a setting equal to
  the font's default (only `liga` defaults on) is dropped from the list
  rather than stored. Whether a feature *renders* depends on the font:
  the pinned Noto Sans subsets have no `smcp`, so no test asserts glyphs.
- **Font family chooser**: the database's families (`FontDb::families`,
  sorted) once enumerated, empty until then (the tool never waits for
  enumeration from the infobar); the tool remembers the list it offered so
  a chosen index resolves to the family shown. A chosen family becomes
  `TypefaceRef { family, full_name: family, panose: None }`. A substituted
  family still shows its own name.
- **Line spacing field**: per cent of the ratio, or points when the
  paragraph already spaces absolutely; the typed number is read in the
  field's current mode (no way to switch mode from the bar yet).

Tests: `xarast-doc` `text_edit::tests` (+3: a character attribute styles
exactly its range and typing after it; a paragraph attribute on every line
of the touched paragraphs, also after a break was deleted; non-text values
and bad ranges refused), `xarast-format` `tests/svg_text_features.rs` (2),
`xarast-app` `text_infobar::tests` (3) and `tests/text_infobar.rs` (8, through
`Session` intents), `xarast-ui` `tests/text_bar.rs` (4, AccessKit) and
`text_ruler::tests` (5, two through the canvas widget's raw input).

## Convert to shapes (W9.6, as built, XARA-US-0049)

Code: `xarast-doc/src/text_convert.rs` (`convert_story_to_shapes`,
`OutlineRun`, `is_text_slot`), `xarast-app/src/convert.rs`
(`ConvertCommand`, `convert_nodes`), `xarast-app/src/text.rs`
(`story_outlines`), `GroupNode::source_text`.

- **Split across crates because of layering.** The phase doc puts
  `ConvertTextToShapes` in `xarast-doc`, which has no fonts. The layout
  half lives in `xarast-app` (`story_outlines` = `build_story`, the
  walker's own geometry: one `BezPath` per attribute run, document space,
  faux italic and underline included); the tree half is
  `xarast_doc::convert_story_to_shapes`, fed `OutlineRun { path, attrs }`.
  Because the geometry is literally the walker's, the render cannot drift:
  **zero differing pixels** on every `TextDesigns` story.
- **What the tree gets.** A `Group` at the story's place (`Attach::Prev`,
  then the story is deleted, retained by the undo step) with
  `source_text` = the story's layout text (`StoryText::text` without the
  final EOL; paragraphs separated by `'\n'`). One `Path` (filled and
  stroked flags on) per run; its own attribute children are **the non-text
  slots whose run value differs from `resolve_inherited(story)`**, so a run
  paints exactly as it did and nothing redundant is written. Text slots
  (`Txt*`) are dropped. **Winding is forced non-zero** (glyphs always fill
  non-zero in the walker) whenever the inherited rule is not. The story's
  own *slotless* attributes (object name, user attributes) move to the
  group. Multi attributes of the runs are not copied (the walker ignores
  them on text).
- **A story with no ink** (only spaces, or no font at all) is left alone,
  not replaced by an empty group.
- **Text on a path** converts to outlines along the path (the walker's
  fitted geometry), and the path it followed goes into the group first,
  with the non-text attributes it painted with, as the original keeps it
  (`Kernel/nodetxts.cpp:1791-1806`). `Designs/TextCurve.xar`: every story
  converted, 0 differing pixels, exact undo (XARA-T-0246).
- **Source text in `.xarast`**: `<g xarast:kind="group"
  xarast:was-text="true">` with a first child
  `<xarast:text-source>…</xarast:text-source>` (`research/06 §6.7`);
  the reader takes it back into `source_text` and skips the element as a
  child. In the canonical digest it is appended only when present, so
  every existing group digest is unchanged.

Evidence (`xarast-app/tests/text_convert.rs`, pinned fonts, CPU backend,
800 × 600, frame = the drawing): all 14 `TextDesigns` files, **54
stories**, convert with **0 differing pixels**; the outlines' tight box
equals the walker's text ink box within 1 mp; one undo step; undo
restores the canonical digest and the render exactly; the converted
document saved as `.xarast` and reopened renders identically and every
group keeps its text. Synthetic: a two-colour story in a group on an
even-odd layer → group of 2 paths with non-zero winding and the name on
the group; selecting the story selects the group it became.

## Walker integration (as built, round 2)

- **Model (`xarast-doc/src/text_model.rs`).** `StoryText::collect(tree,
  story, &mut AttrStack, attr_value)` walks one story with the render
  walk's attribute stack (so each character resolves exactly as it would
  paint) and returns `text`, `runs: Vec<CharRun { range, attrs:
  ResolvedAttrs }>` (a new run only when an attribute node was pushed or a
  scope popped), `lines: Vec<LineEntry>` (line-level attributes = the
  snapshot at the line's first item), `items` (the `TextPos` index),
  `kerns: Vec<KernAt { at, amount }>`. `byte_of(TextPos)` / `pos_of(byte)`
  are binary searches. `StoryFlow::of(story)` reduces `TextLayout`. An
  item's own attribute children apply to it (painted at the end of their
  scope). Nothing is written to the tree.
- **Bridge (`xarast-app/src/text.rs`).** `story_input` is the table above;
  the typeface's PANOSE now travels in `StyleRange::panose` so the ladder's
  PANOSE rung works during layout. The **last** `'\n'` of a story is not
  given to layout: every `.xar` line ends in an EOL, and a trailing break
  would open an empty paragraph the original never measures (it made
  `hebrew.xar`'s frame twice as tall).
- **Geometry.** `build_story` lays out, then per glyph applies
  `story.transform × glyph_transform` (plus a shear for faux italic when
  `FontMatch::synthesis.skew` says so) and appends the outline to **one
  `BezPath` per style run**, then converts to a millipoint `Path`. Glyph
  outlines fill **non-zero**, whatever the winding attribute. Underline is
  a rectangle a tenth of the size below the baseline, a twentieth thick
  (not the font's `post` values yet).
- **Walker.** A `TextStory` is painted whole at its `Visit` and its
  subtree skipped. Each run emits fill / stroke / transparency like any
  object, so gradients (document-space geometry) span the story as in the
  original. The `StoryGeometry` is cached per `NodeId` and dropped when
  `Document::epoch` moves; its content hash folds every node version in the
  story. `WalkStats` gains `text_stories`, `text_on_path_pending`;
  `text_pending` now means "visible text with no inked glyph" (no font at
  all). `SceneWalker::text_ink()` is the drawn text's box: `Session` adds
  it to `scene_ink`, and `viewport::drawing_rect_with` lays stories out for
  framing (text has no cached bounds).
- **Fonts (`xarast-app/src/fonts.rs`).** `FontService { FontDb, Shaper,
  OnceLock<usize> }`; `ready()` enumerates once (on the caller's thread if
  nobody started it). `fonts::shared()` is the process service: system
  fonts, or `XARAST_FONT_DIR` / `set_shared(FontService::from_dir(dir))`
  for pinned fonts (all generics → "Noto Sans", every family a fallback for
  Hebrew/Arabic/Han/kana). The shell calls `fonts::prewarm()` before the
  window. Tests pin fonts: `xarast-app/tests/{corpus,text,xarast_roundtrip}.rs`
  and `xarast-cli/tests/cli.rs` (env var).
- **Substitutions are never silent.** The walker records each once
  (`font_substitutions()`); `Session::take_font_substitutions` hands out the
  new ones; `AppState::collect_font_substitutions` puts them in the problem
  list as warnings and the viewer shows the newest in the status bar; the
  CLI prints `font substituted: A -> B` on stderr.
- **Importer (`xarast-xar`).** A string's/character's attribute children
  apply to **it alone** (`Kernel/cxftext.cpp:1260-1300`: a string is a run
  of characters with identical attribute children; `Kernel/impstr.cpp`
  copies them to every character). They were emitted after the characters,
  which handed each string's style to the next one (visible in
  `FontChangesInText.xar`). Now they go before, and what they overrode is
  restored before the next text record that does not set the slot; the
  mapper mirrors the emitted attribute state (`AttrStack`) to know that
  value. Non-BMP characters written as two `TAG_TEXT_CHAR` records pair.
  A story that relies on the original's default size gets an explicit 16 pt
  attribute (below).

### The acceptance fixture, round 2 (pinned: this machine's fonts)

Every `TextDesigns` file carries a **bitmap of the original's own
rendering** under the text (the "red/green ghost" in our renders), which
makes each file a visual oracle. With the host's metric-compatible fonts
(Arial → Liberation Sans) `SimpleText`, `Paragraph`, `TextJust`,
`LineSpacing`, `BaselineShift`, `SuperSub`, `Tracking`, `ManualKern` and
`Kerning` overlay their reference within about a pixel at 250 %.
`FontChangesInText`, `Rotated` and `AngledText` differ only where Calisto MT
/ Book Antiqua were substituted (Noto Serif is wider). `hebrew.xar`'s story
has no typeface (default Times New Roman); Windows' Times New Roman has
Hebrew, Liberation Serif does not, so the fallback face is larger and
serif. `embeddedFonts.xar` embeds **no** font data: its Calligraphic,
Margaret and Myriad Web are substituted.

Missing on this machine: Arial, Times New Roman, Courier New (metric
aliases used: Liberation Sans/Serif/Mono), Calisto MT, Book Antiqua (→
Noto Serif via PANOSE), Calligraphic, Margaret, Myriad Web (→ Noto Sans),
KirbysHand and Swis721 Blk BT (defined in files, never reached a laid-out
run).

### TextDesigns acceptance gate (W9.7, XARA-T-0260, as built)

`crates/xarast-cli/tests/text_designs.rs`, over the 14 files the corpus
lock lists under `TextDesigns/` (skips without `XARAST_XAR_CORPUS`):

- **Criterion 3.** `xar-dump --tags` on each file; a row whose name carries
  the `!` (no decoder) inside 2100-2117, 2200-2204, 2900-2920 or 4200-4207
  fails. None does.
- **Criterion 2.** `xarast-cli render <dir> --out-dir <tmp> --zoom 200`
  with `XARAST_FONT_DIR` = the pinned set: exit 0, `14 rendered (14 with
  ink)`, no `[not drawn: …]`, and each PNG's
  `xarast_render::golden::digest` (SHA-256 of size + RGBA) equals the row
  in `crates/xarast-cli/tests/golden/text_designs.sha256` (gate A, exact).
  `XARAST_UPDATE_GOLDEN=1` rewrites the file; a mismatch keeps the render
  in the temp directory and prints its path.
- **No missing-glyph boxes.** Each story's layout through
  `text_tool::caret_map(..).layout()` (the walker's own `lay_story`): no
  glyph id 0 on a non-space, non-control character.
- **Criterion 12 / T9.7.3** is `xarast-app/tests/xarast_roundtrip.rs`
  (all 59 files, these 14 included, zero differing pixels).

**Golden policy for corpus renders.** The files are Xara's artwork and
each carries a bitmap of the original's own rendering, so neither they
nor our renders of them are committed (the PNG goldens of
`xarast-render/tests/golden/` are our own synthetic scenes). A corpus
golden is a **digest plus the size**: derived, small, and exact. It is a
regression gate only: with the pinned set every family here (Arial, Times
New Roman, Calisto MT, Book Antiqua, Calligraphic, Margaret, Myriad Web)
falls to Noto Sans, wider than Arial, so the renders do *not* overlay the
reference bitmaps. Gating against those (metric-compatible pinned faces,
gate C) is XARA-T-0262. 200 % rather than 100 % so a sub-point move
changes pixels; the whole test runs in ≈ 0.5 s (release deps) and the
render in ≈ 1.2 s debug.

**Tag inventory (T9.7.1)** — text tags per file, from `xar-dump --tags`:

| File | Text tags |
|---|---|
| AngledText | 2100 2101 2200 2201 2203 2900 2906 2907 2908 2910 2918 |
| BaselineShift | 2100 2200 2201 2203 2900 2906 2907 2919 2920 |
| FontChangesInText | 2100 2200 2201 2203 2900 2906 2907 2919 2920 |
| Kerning | 2100 2200 2201 2202 2203 2204 2907 |
| LineSpacing | 2100 2200 2201 2202 2203 2900 2906 2907 2919 2920 |
| ManualKern | 2100 2200 2201 2203 2204 2900 2906 2907 2919 2920 |
| Paragraph | 2100 2200 2201 2202 2203 2900 2906 2907 2919 2920 |
| Rotated | 2101 2200 2201 2202 2203 2204 2900 2903 2904 2905 2906 2907 2916 2917 2918 2919 2920 |
| SimpleText | 2100 2200 2201 2203 2906 2907 |
| SuperSub | 2100 2200 2201 2202 2203 2900 2906 2907 2916 2917 2919 2920 |
| TextJust | 2100 2200 2201 2203 2903 2904 2905 2906 2907 |
| Tracking | 2100 2200 2201 2202 2203 2900 2906 2907 2918 2919 2920 |
| embeddedFonts | 2100 2200 2201 2203 2906 2907 |
| hebrew | 2101 2200 2201 2203 2900 2906 2907 2919 2920 |

Tag names: 2100 STORY_SIMPLE, 2101 STORY_COMPLEX, 2200 LINE, 2201 STRING,
2202 CHAR, 2203 EOL, 2204 KERN, 2900 LINESPACE_RATIO, 2903/2904/2905
JUSTIFICATION_CENTRE/RIGHT/FULL, 2906 FONT_SIZE, 2907 FONT_TYPEFACE, 2908
BOLD_ON, 2910 ITALIC_ON, 2916/2917 SUPERSCRIPT/SUBSCRIPT_ON, 2918
TRACKING, 2919 ASPECT_RATIO, 2920 BASELINE. Nothing in 4200-4207 occurs.
Where the phase table's "exercises" column differs from the inventory:

- **Kerning.xar carries manual kerns** (2204, as ManualKern does); its
  automatic kerning is the story's auto-kern flag, not a tag.
- **AngledText.xar**: no attribute tag carries character rotation or
  shear (none exists in 2900-2920); the angles travel in the story
  records (2100/2101). It also exercises bold, italic and tracking.
- **Rotated.xar** is the broadest file: every justification, super/sub,
  tracking and kerns, besides the story matrix.
- **LineSpacing.xar** uses only the ratio form (2900); no absolute line
  spacing tag occurs in any of the 14.
- **embeddedFonts.xar** has no font data (already noted above), so the
  embedded-face path (criterion 5) is untested by the corpus.

## Facts about the original's formatter (read, not copied)

- **The em of tracking and manual kerns is the advance of `'M'`**, not the
  point size: `FontEmWidth` is the cached width of `FONTEMCHAR`
  (`wxOil/textfuns.h:114`, `wxOil/fontbase.cpp:945-956`), scaled by the
  aspect. About 0.83 em for Arial, 0.907 for Noto Sans. `FaceMetrics::
  em_char_advance` reads it; layout scales `StyleRange::em_width()` by it.
  Before this, `Tracking.xar` was ~20 % too wide; now it overlays.
- **The default font size is 16 pt** (`Kernel/txtattr.cpp:449-452`); the
  default typeface is "Times New Roman" (`Kernel/fontman.h:111`). The
  model's `default_for(TxtFontSize)` is 16 pt since XARA-T-0172 (the
  `.xarast` writer always writes a run's size and the reader's fallbacks
  use `default_for`); the importer's explicit 16 pt is no longer needed
  and no longer written.

Taken from the original to fix semantics; implemented from these notes.

- **Tracking unit** (settles `research/01 §11` item 15 and
  `xar-import.md` open question 1): `AttrTxtTracking` holds thousandths of
  an em. The advance is `CharWidth + MulDiv(tracking, FontEmWidth, 1000)
  (+ autokern)` (`Kernel/nodetext.cpp:1781-1792`); `FontEmWidth` is scaled by
  the X scale, i.e. it includes the aspect ratio (`wxOil/textfuns.cpp:151-157`).
- **Manual kerns** (`KernCode`) are thousandths of an em too:
  `MulDiv(KernValue.x, FontEmWidth, 1000)` (`Kernel/nodetext.cpp:1763-1767`).
  The importer stores `TextItem::Kern(Mp)` with that raw value — it is em/1000,
  whatever the type says.
- **Line width for alignment** is the sum of full advances up to the last
  non-space character, plus that character's *width* (advance without
  tracking or autokern); trailing spaces do not count
  (`Kernel/nodetxtl.cpp:1438-1491`).
- **Alignment** (`Kernel/nodetxtl.cpp:1530-1579`): left starts at the left
  margin; right at `right − width`; centre at `(left + right − width) / 2`
  (C integer division). Physical right = story width for a column, 0 for
  point text, so point text aligns about its anchor.
- **Full justification**: if the line wraps, is the last of its paragraph
  and is short, it is left aligned. Otherwise `gap = (right − left) −
  width`; if `gap > 0` and the line has spaces, every space gets
  `gap / spaces`; if `gap > 0` without spaces (and the story wraps), or
  `gap ≤ 0`, every character gets `gap / (chars − 1)` (letter spacing can
  shrink). Counts stop at the last non-space character and **restart after
  each tab**: only the last tab section stretches. "Characters" includes
  spaces.
- **First line of a paragraph uses the first-line indent *instead of* the
  left margin** (`Kernel/nodetxtl.cpp:780-795`), not in addition.
- **Line breaking** (`FindBreakChar`, `Kernel/nodetxtl.cpp:1262-1362`):
  spaces never overflow and are break points; a break is allowed after a
  hyphen and before a tab; a character fits if its width (no tracking) fits;
  at least one character per line; an overlong word breaks before the
  character that overflows. Our UAX #14 opportunities are a superset of the
  original's (they add CJK and the like).
- **Line spacing** (`CalcBaseAndDescentLine`, `Kernel/nodetxtl.cpp:1600-1640`):
  with `h = ascent + descent` of the line; **absolute** `S`: each line box
  is `S` tall and the baseline sits `S × descent / h` above the box bottom;
  **ratio** `r`: the box is `h × r` tall and the baseline sits
  `ascent × min(r, 1)` below the previous box's bottom. The first baseline
  is 0. `LineSpacing::Absolute(0)` means "use the ratio".
- **Default tab stops** every 36 000 mp (half an inch), strictly after the
  current position (`Kernel/txtattr.cpp:3181-3205`).
- **Ruler record** (`Kernel/rechtext.cpp:1546-1575`, writer
  `Kernel/cxftext.cpp:2475-2515`): `u16` count, then per stop `u8
  type_and_flags`, `i32` position, a `u16` decimal-point character **if
  the type is decimal**, a `u16` filler character **if bit 2 is set**.
  `type = flags & 3` is the original's in-memory enum `{LeftTab,
  RightTab, CentreTab, DecimalTab}` (`Kernel/txtattr.h:143`): 0 left, 1
  right, 2 centre, 3 decimal. Imported since XARA-T-0263 (no corpus file
  has the record); the import keeps `kind = flags & 3` and drops the
  filler (flag and character: the model has no filler yet).

## Tab sections: right, centre and decimal stops (XARA-T-0263)

Facts from `TextLine::FindBreakChar`, `FormatState::{AdvanceBy,
IsAvailable, FinishTabSection}` (`Kernel/nodetxtl.cpp:1005-1500`), with
`TxtRulerAttribute::FindTabStopInRuler` (`Kernel/txtattr.cpp:3181`); the
implementation is `FormatState` and `break_lines` in
`xarast-text/src/layout.rs`.

- **A section** is the text after one tab up to the next tab, the end of
  the line (wrap or paragraph end). The text before the first tab is
  anchored to a notional left stop at the line start. A tab takes the first
  stop **strictly beyond** the current right edge (`Width`, which starts at
  the indent), so two tabs in a row advance; past the last ruler stop the
  stops are left stops every 36 000 mp.
- **Right**: the section's characters fill the room between the tab and the
  stop from its right end; all of the section ends at the stop. **Centre**:
  half of each character's width (C-truncated `w / 2`) goes into the room,
  the other half to the right of the stop. **Decimal**: like right until
  the decimal character is met; that character and everything after it go
  to the right of the stop. The decimal point is always right of the stop.
- **The tab's width is the room left when the section ends**
  (`FinishTabSection`), so the text of a right/centre/decimal section is
  placed by shrinking the tab; the width of the line is unchanged by
  alignment. Our `tab_advances` take that final value.
- **Overflow**: when the room is used up (`remaining == 0`), the rest of
  the section goes to the right of the current edge, so the tab's width is 0
  and the text pushes **past** the stop; nothing is clipped or moved to a
  later stop. For centre, half of a character that does not fit in the room
  uses what room remains and the rest goes right.
- **Decimal stop with no decimal character in the section**: the section is
  never "found", so it behaves exactly as a right stop. A decimal stop's
  character is compared with the first scalar of each cluster; only a
  decimal stop compares (a `.` in a right section is ordinary text).
  `DecimalPointFound` resets at each tab.
- **Wrapping**: whether a character fits is decided by placing it on a copy
  of the state (`IsAvailable` "advance and backtrack"): a right section
  that still lies left of the stop does not push the line toward the
  limit. A line broken mid-section finishes that section **at the break**
  (the tab's width reflects only the characters kept; we keep a snapshot of
  `remaining` per cluster). A line may break before a tab. Divergence
  kept from before: the original also refuses a tab whose stop lies beyond
  the wrap limit (breaks before it); we let the tab through.
- **Justification**: alignment of the whole line (left/centre/right) uses
  the summed advances, which already contain the final tab widths, and
  tab positions are relative to the indent not the shift (`CharPosOffset` is
  "transparent" to stops). Full justification stretches only the **last**
  section (counts restart after each tab, as before), so the gap is
  measured against the line's total including shrunken tabs.
- **Model**: `xarast_doc::TabStop { position, kind: u8, decimal:
  Option<char> }` and `xarast_text::TabStop { pos, kind, decimal: char }`.
  `None` (a stop from a file that did not state one, or the ruler UI before
  the user picks one) lays out with a full stop; the ruler's "add decimal
  stop" writes `Some('.')`. `.xarast` ruler text: `position:kind` plus an
  optional third field, the code point in decimal (`72:3:46`); stops
  without it, and files from before the field, are unchanged byte for byte.
- **Corpus**: no corpus file has a `TAG_TEXT_RULER` record, so every corpus
  story uses the default left grid; its layout is unchanged by this work
  (the golden-image and text tests pass untouched).
- Line ascent/descent/size are the maxima over the line's characters; an
  empty line takes them from its end-of-line item.

## The contract for W9.2 (`xarast-doc`, next round)

What the document layer builds, and how `format_story` feeds `Shaper`. The
existing model (`crates/xarast-doc/src/text.rs`, `attr/`) already has most
of the structure; this is what to add and how to bridge.

### Nodes (existing, keep)

- `NodeKind::TextStory(Box<TextStoryNode { transform, layout: TextLayout,
  auto_kern, print_as_shapes }>)`, `NodeKind::TextLine(Box<TextLineNode {
  ruler }>)`, `NodeKind::TextItem(TextItem::{Char(char), Kern(Mp),
  Tab, LineBreak(bool)})`. Invariant 11: items only under lines, lines only
  under stories.
- Lines are **formatted lines**; `LineBreak(true)` is the paragraph end,
  word wrap produces lines without one. Lines are derived state: undo
  restores the item list and reflows (phase doc W9.2). Proposed diff
  threshold: 4 KiB of items per paragraph, above which the inverse stores a
  diff; not implemented yet.

### Add to the model

- `TextPos { line: NodeId, item: u32 }` and the story's **logical text**:
  the concatenation of the items in document order where `Char(c)` → `c`,
  `Tab` → `'\t'`, `LineBreak(true)` → `'\n'`, `LineBreak(false)` → nothing
  (it is not produced by the importer and should not survive reflow), and
  `Kern` → nothing (it becomes a `ManualKern` at the byte offset of the
  next character). Conversion `TextPos ↔ byte offset` walks this mapping;
  keep a per-story `Vec<(NodeId, u32 /*first byte*/)>` index, rebuilt on
  format, so it is O(log n).
- `TextCursor { story, anchor: TextPos, head: TextPos }` in the tool's state
  (not in the tree).
- Derived per-line cache, outside the arena: the `LaidLine` of each
  `TextLine` (baseline, ascent/descent, glyph runs) keyed by line node, plus
  the story's `Layout`. The arena keeps no metrics.

### Attribute bridge (`AttrValue` → `StyleRange` / `ParagraphStyle`)

Resolve the attribute stack once per run of identical character attributes
(Xara's `FormatRegion` role) and emit one `StyleRange` per run, byte ranges
in the logical text:

| Attribute | Goes to | Conversion |
|---|---|---|
| `FontTypeface(Arc<TypefaceRef { family, full_name, panose }>)` | `FontQuery.family` | `family` (not `full_name`); pass `panose` to `FontDb::query_with_panose` |
| `Bold(b)` / `Italic(i)` | `FontQuery.weight` / `.style` | 700/400, `Italic`/`Normal`; stretch 100 |
| `FontSize(Mp)` | `StyleRange.size` | as is |
| `Tracking(Mp)` | `StyleRange.tracking: i32` | **raw value, it is em/1000** (the `Mp` type is historical) |
| `AspectRatio(f32)` | `.aspect` | as is |
| `Baseline(Mp)` | `.baseline_shift` | as is, positive up |
| `Script(Script { on, offset, size })` | `.script: TextScript` | `on == false` → `TextScript::NONE`, else `{ offset, size }` |
| `Underline(b)` | `.underline` | as is |
| `Justification` | `ParagraphStyle.justification` | same four values |
| `LineSpace(LineSpacing::{Ratio, Absolute})` | `.line_spacing` | same shape |
| `LeftMargin` / `RightMargin` / `FirstIndent` | `.left_margin` / `.right_margin` / `.first_indent` | as is |
| `Ruler(Arc<[TabStop { position, kind: u8, decimal: Option<char> }]>)` | `.tabs` | `kind & 3`: 0 Left, 1 Right, 2 Centre, 3 Decimal; `decimal` → `TabStop.decimal` (`None` → `.`) |
| `TextStoryNode.auto_kern` | every `ParagraphStyle.auto_kern` | story-wide |
| `TextItem::Kern(Mp)` | `ManualKern { at, amount }` | `amount` = raw value (em/1000), `at` = byte offset of the following character |

Line-level attributes live under `TextLine` nodes. A paragraph takes its
`ParagraphStyle` from its **first** line (the lines of one paragraph carry
the same line-level attributes); `StoryInput::paragraphs` has one entry per
`'\n'`-separated paragraph.

### Story mode bridge

| `TextLayout` | `StoryMode` |
|---|---|
| `AtPoint` | `Point` |
| `InColumn { width, word_wrap }` | `Column { width, wrap: word_wrap }` (a non-wrapping column still aligns to its width) |
| `OnPath { .. }` | `PathFit::story_mode()`: a non-wrapping `Column` as long as the path minus both indents, then fit to the path (W9.5, below). `Point` when the path is missing |

`TextStoryNode.transform` (the `.xar` story matrix) is applied by the
renderer on top of the story-space layout; `Layout` never includes it.

### What the `.xar` importer already keeps (W3.10, "structure only")

From `crates/xarast-xar/src/import.rs`: stories (2100/2101/2110–2117 with
the matrix and auto-kern), `TAG_TEXT_STORY_WORD_WRAP_INFO` → `InColumn`,
indents, lines (2200), strings (2201) and chars (2202) → `Char`, EOL (2203)
→ `LineBreak(true)`, tab → `Tab`, kern → `Kern(dx)`, and the 2900–2920 and
4201–4204 attributes above. Font definitions (2000/2001) become
`TypefaceRef` with PANOSE. `TAG_TEXT_LINE_INFO` (2206, the original's cached
line metrics) is skipped — our layout recomputes it; it could serve as a
cross-check oracle in W9.7. Two importer issues for W9.2.6:

1. `TAG_TEXT_CHAR` is one UTF-16 code unit and is mapped with
   `char::from_u32(u16)`, so a non-BMP character stored as two `TEXT_CHAR`
   records becomes two U+FFFD. Pair surrogates across consecutive records.
2. The `Tracking` and `Kern` values are em/1000 (above); the conversion
   happens in `format_story`, so the import can stay raw, but the doc
   comments that call the unit "unsettled" should be updated.

### `format_story` sketch

```text
format_story(tree, story, shaper):
    text, index, runs, paras, kerns = collect(story)   // attribute resolution
    layout = shaper.layout(&StoryInput { text, runs, paras, kerns, mode })
    restructure lines to match layout.lines (wrap = move items between
        TextLine nodes; never touch LineBreak(true))
    cache layout per line; return substitutions for the UI
```

## Text on a path (W9.5, as built, XARA-US-0048)

**Architecture open question 4 — decided 2026-09-24: no layout pass of our
own.** Parley's output (through our own line layout) is laid out straight
and then carried onto the path by arc length: spike A of the phase doc,
implemented in `xarast-text/src/path.rs` (`TextPath`, `PathFit`,
`PathFitStyle`). Spike B (layout-aware spacing) exists only as the
comparator inside the measurement test and is not shipped.

**The measurement (T9.5.3)**, `xarast-app/src/text_path_fidelity.rs`,
pinned fonts, per glyph origin displacement over the glyph's advance,
against the original's rule evaluated independently (a 0.25 mp polyline
walk, extension along the end control points). Pass = p95 ≤ 0.25 and max
≤ 0.5:

| Fixture | Glyphs | A p95 / max | B p95 / max |
|---|---|---|---|
| Tight open arc (r = 24 pt, 12 pt text inside) | 18 | 0.0021 / 0.0021 | 3.14 / 3.14 |
| Closed circle, full justification (r = 60 pt, 14 pt) | 51 | 0.0013 / 0.0014 | 5.29 / 5.81 |
| `Designs/TextCurve.xar` (3 stories) | 1 388 | 0.0007 / 0.0015 | 1.30 / 3.32 |
| `TextDesigns/AngledText.xar`, `Rotated.xar` | 0 on a path | 0 / 0 (the fit never runs) | 0 / 0 |

A passes on all five fixtures, so by the phase doc's rule A wins. There is
no rendering of the original to compare with for these files (the
`TextDesigns` bitmaps are straight text), so the reference is the
original's documented rule; B fails *because* the original does not
respace: any layout-aware pass moves glyphs away from where it puts them.

**Known limitation, kept on purpose.** On the inside of a tight curve the
tops of letters crowd and can touch, exactly as in the original; spacing
them apart (spike B) is a deliberate departure to consider only as an
opt-in style (phase 15), never the default.

Facts about the original (read, not copied):

- **Width and margins.** A story on a path formats with `StoryWidth` =
  the path's length; the physical left margin is the left indent and the
  physical right margin is the length minus the right indent
  (`Kernel/nodetxts.cpp:2716-2731`, `Kernel/nodetxtl.cpp:815-816`). So
  alignment and full justification work along the path. We lay out a
  `Column { width: length − indents, wrap: false }` and add the left
  indent to the distance. Word wrap on a path (possible in the original)
  is not modelled: `OnPath` has no wrap flag and the corpus never wraps
  (decided a non-goal in T9.5.5, see "Editing text on a path").
- **Placement** (`TextLine::FitTextToPath`, `Kernel/nodetxtl.cpp:1686-1755`):
  distance = position in line + half the character's *width* (its advance
  without tracking or kerns); point and tangent at that distance; the
  character's local frame (origin at its left end on the baseline) is
  pre-transformed (x by |scale| × aspect, shear, y by scale), turned by
  the tangent angle about (half width, 0), and translated so that point
  lands on the path. `LaidCluster::pen` and `::advance` were added for
  this.
- **Closed paths** wrap the distance modulo the length; **open paths**
  extend past either end in a straight line along the end direction
  (`Kernel/pathproc.cpp:965-1030`: the last two coordinates of the path,
  so for a curve its end tangent). The original walks a 64 mp flattening
  and takes the tangent of the flattened segment; we invert kurbo's exact
  arc length (0.01 mp accuracy) and take the exact derivative. Difference
  < 64 mp in position.
- **Lines after the first** are offset by (baseline × scale) along the
  unit normal of the path's **start** direction (first two coordinates),
  the same vector for every character of the line — not along each
  point's normal. Parallel copies of the curve, so lines can meet where
  the curve turns towards that normal.
- **The path is the story's first `NodePath` child**
  (`Kernel/nodetxts.cpp:1932-1935`), stored in document space; the
  story's matrix is removed from a copy before fitting (reversed first
  when the text is reversed), so doc = story matrix × fitted character
  (`CreateUntransformedPath`, `Kernel/nodetxts.cpp:3242-3316`).
- **The path renders** as an ordinary child (its own attributes, the
  story's before it); `TextCurve.xar`'s own instructions say to hide it
  with "no colour". The walker paints it under the text
  (`paint_story_path`).
- **The eight on-path records** (`Kernel/cxftext.cpp:671-700`,
  `Kernel/rechtext.cpp:335-540`): START_LEFT plain, END_RIGHT reversed,
  START_RIGHT reflected (a negative `CharsScale`), END_LEFT both; the
  COMPLEX forms add a full story matrix plus `CharsRotation` and
  `CharsShear` (`ANGLE`, 16.16 radians). The importer used to read
  "reversed = odd tag", which was wrong for 2111/2112/2115/2116 (none in
  the corpus). The model keeps these as `TextLayout::OnPath::chars:
  CharsTransform`; `CharsRotation` is carried but not drawn — the
  original's own fit has it commented out.
- **`tangential: false`** (upright characters) is supported by the fit;
  the original asserts it is always true ("not yet supported").
- The large horizontal kern at a line's start is how the original lets a
  user click where text on a path starts (`TextCurve.xar` says so); it is
  an ordinary manual kern (em/1000), nothing special in the fit.

Evidence: `xarast-text` `path::tests` (8: arc length, reversal, wrapping,
collapsed handle, degenerate paths, turning about the centre, upright,
indents and reflection) and `tests/layout.rs`
`a_cluster_knows_its_glyph_pen_and_its_own_advance`; `xarast-app`
`tests/text.rs` `text_on_a_path_follows_its_path_and_the_path_is_painted`,
`tests/corpus.rs` (no story drawn straight), `tests/text_convert.rs`
`text_on_a_path_converts_to_outlines_along_the_path`; `xarast-format`
`a_story_on_a_path_keeps_its_parameters_and_its_path`; `xarast-xar`
`the_eight_on_path_tags_say_reversed_and_reflected`.

### Carets on a path (XARA-T-0250, as built)

Code: `xarast-app/src/text_edit.rs` (`CaretMap::on_path`, `place`,
`caret_segments`, `selection_quads`, `hit_point`; `Stop::cluster`),
`text_tool.rs` (`StoryView`).

- **A stop belongs to a cluster** (`Stop::cluster`, the index in the
  line's clusters). The caret is drawn at *its own* cluster's edge, not
  "at x on the line": on a curve the right edge of one cluster and the
  left edge of the next are two different points (the boxes turn apart
  on the outside of a bend, overlap on the inside). Affinity picks which:
  downstream = the next cluster's left edge, upstream = the previous
  one's right edge, as for soft line ends.
- **Pieces.** Each line is cut into rigid pieces of straight-layout x,
  each with one transform: a cluster's box under
  `PathFit::cluster_transform`, except that a gap in the box longer than
  both the glyphs' advance and half the line's size (a manual kern, e.g.
  `TextCurve.xar`'s long kern at a line's start) is cut into chunks no
  longer than that, each fitted as a character of its own
  (`span_transform`). Without this the caret before a long kern stuck out
  along the first glyph's tangent, off the path. An empty line is one
  zero-width piece at its start. Pieces (and their inverses) are built
  once per layout.
- **Hit test on a path**: every piece inverse-maps the point; the score
  is (distance outside the box — the larger of across and along —, then
  how deep inside along x), lowest wins, so where neighbouring boxes
  overlap on the inside of a bend the one the point is deeper in wins.
  The caret goes to the nearer edge of the winning piece's cluster. The
  distance is also "is the click on this story" (4 px, decision 54 of
  `tools.md`). Straight text keeps the old rule (nearest line band, then
  nearest stop) and the old Chebyshev distance to the line boxes.
- **Selection**: one quad per selected cluster, left corners from its
  first piece and right corners from its last (so a kerned cluster is
  still one quad), plus one for a selected paragraph break fitted as a
  character of its own. Spans are not merged on a path (a merged span
  cannot bend).
- **The caret leans with shear**: the segment is the fitted glyph's own
  vertical, so a sheared story (`CharsShear`) gets a slanted caret, and a
  reflected one a caret hanging on the other side.

Tests: `xarast-app/tests/text_path_caret.rs` (6; all but the last on
`Designs/TextCurve.xar`, 3 stories): every stop of every line is the
fitted cluster edge within 1 mp, on the glyph's baseline and parallel to
its vertical (> 1000 stops, most of them turned); a click at 20 % / 80 %
of every glyph gives the nearer edge; every stop round-trips through a
click a quarter of its cluster inwards; Right walks every boundary in
logical order and the caret moves at most a cluster along the path per
step; through `Session` intents a click on a glyph, Right, and a 3-character
selection drawn as 3 turned quads; and a synthetic half-circle arc
(no corpus).

**Dead end:** asserting a round trip 0.2 pt inside a cluster's edge fails
on `TextCurve.xar` line 7: with tight tracking (advance 14.1 pt in a
12.4 pt box) and a hard bend, the neighbour's fitted box covers that
point too. Both carets are the same offset; the test clicks a quarter of
the cluster inwards instead.

### Editing text on a path (T9.5.5, as built, XARA-T-0251)

Code: `xarast-app/src/text_path_edit.rs` (`TextPathEdit`, run as
`EditCommand::TextPath`, so through the bus and the journal),
`text_tool.rs` (indent handles), `node_edit.rs::edited_paths`,
`xarast-text` `TextPath::project`.

Facts about the original (read, not copied; `tools/textops.cpp`
`OpFitTextToCurve`, `OpReverseStoryPath`, `OpDragStoryPath*Indent`,
`Kernel/nodetxts.cpp` `MatrixFitToPath`/`MatrixRemoveFromPath`/`Transform`/
`GetLeftIndentPos`, `Kernel/matrix.cpp` `Decompose`/`Compose`,
`Kernel/textacts.cpp` `ReversePathAction`, `wxOil/xrc/SHARED_MENU.res`,
`STANDARD_HOTKEYS.res`):

- **One menu item toggles** ("FitTextToPath", last item of Arrange): it
  fits when exactly one story (not on a path) and exactly one object that
  can become a path are selected, and removes when the selected story is
  already on a path. No key. The object is moved into the story as its
  first child and converted to a path if it is not one; the selection
  becomes the story. Before moving it, the original **translates the story
  so its bounds' low corner meets the object's** ("so that attributes and
  effects are in roughly the right position").
- **MatrixFitToPath**: decompose the story matrix into scale (signed;
  negative = mirrored), aspect, rotation (atan2 of the x axis), shear and
  translation, keep them as `CharsScale/Aspect/Rotation/Shear`; if the
  rotation is past ±90° toggle "reversed" and turn the rotation by π
  (towards 0); the story matrix becomes the translation alone.
- **MatrixRemoveFromPath**: if reversed, turn the rotation back by π and
  toggle reversed; compose scale, aspect, rotation, shear (then the
  remaining story matrix) into the new story matrix. The path moves out as
  the story's **next sibling**; the story's width becomes the path length
  minus both indents **only if it word-wraps**.
- `Compose` is x' = |s|·aspect·x + s·tan(shear)·y, y' = s·y, then rotate,
  then translate. `Decompose` reports the shear with the mirror's sign
  folded in (for s < 0 it gives −shear), which is why `Transform` negates
  it for a negative scale; `MatrixFitToPath` does not, so a mirrored and
  sheared story slants the other way once fitted in the original. We do
  not reproduce that: our split is exact (below).
- `TextStory::Transform` bakes any scale (and, off a path, aspect) into
  font size, line spacing, margins, indents and story width, so the
  original's story matrix normally has |scale| = 1 and aspect 1;
  `Validate` asserts both.
- **Reverse** (`Ctrl+Shift+R`, "ReverseStoryPath") toggles the reversed
  flag of every selected story on a path, nothing else. Greyed when a
  selected story is not on a path.
- **Indents**: two blobs on the path; left at distance `LeftIndent` from
  the path's start (from its end when reversed), right at `RightIndent`
  from its end (start when reversed). A drag snaps the pointer to the
  nearest point of the path, the new indent is that point's distance from
  the start (left) or from the end (right), swapped when reversed, and a
  handle is stopped at the other one. One undoable op per drag.
- Word wrap on a path: the `WordWrapping` flag survives a fit; on a path
  `StoryWidth` is the path length, so a wrapping story on a path breaks
  its lines at the path's length.

Decisions (ours):

- **Commands**: Arrange › "Fit text to path", "Remove text from path"
  (both keyless, as the original) and "Reverse text on path"
  (`Ctrl+Shift+R`, free in our table). Fit and remove are two commands,
  not one toggle, so the menu says what will happen. Remove and reverse
  act on every selected story on a path; fit needs exactly one story and
  one path, shape or quick shape (anything else selected: nothing). Each
  is one undo step ("Fit Text to Path", "Remove Text from Path",
  "Reverse Text on Path", "Path Indent"); nothing applicable records no
  step.
- **The split is exact by construction** (`fit_placement`): rotation θ =
  atan2(b, a), mirror = sign of the determinant, shear φ from the matrix
  with the rotation taken out (upper triangular [[|A|, p], [0, s]],
  tan φ = p / s, which is the fit's own `PathFit` pre-transform); past ±90°
  reversed and θ turned by π. The stored angles are rounded to 16.16 and
  the matrix that stays is the old one **times the inverse of what was
  taken out** (rotation × characters' transform, using the rounded
  angles), so `removed_placement` gives the old matrix back to ~1e-12 and
  the translation exactly. A uniform scale stays in the matrix (the fit
  then scales the text and the path's story-space length together, which
  draws the same); a non-uniform remainder stays too (the original keeps
  aspect in `CharsAspect`, which our model has no field for).
- **No translation onto the path** (deviation): the story keeps its
  translation, so fit then remove draws the story exactly where it was
  (acceptance), which the original's move to the path's corner would not.
- **The path keeps its look** both ways (`move_keeping_look`): before the
  move its resolved non-text attributes are read, and every one that
  would differ in the new place becomes its own attribute (the original
  localises and factors out attributes around the move). It is placed
  after the story's attribute block (no `AttrAfterInk` warning) on fit,
  and as the story's next sibling (in front) on remove.
- **Remove gives a point story** (`AtPoint`), indents dropped.
- **Word wrap on a path: non-goal.** `OnPath` has no wrap flag; a column
  story fitted to a path loses its column (width and wrap) and comes off
  as a point story. Reasons: the corpus never wraps on a path; wrapping
  would need the flag in the model, both writers and the importer for a
  behaviour (lines broken at the path's length, each later line on a
  parallel offset of the start normal) that is rarely what anyone wants
  on a curve. Revisit only with a user request.
- **Indent handles** (text tool): shown while a caret is in a story on a
  path, as `HandleShape::Radius` (a shape's own handle), at the indents in
  the text's direction (`PathFit::path()` already reversed, story space,
  mapped by the story matrix). Grabbed within `HANDLE_TOLERANCE_PX`
  before any text hit. The drag projects the pointer onto the
  story-space path (`TextPath::project`: kurbo's nearest, polished by
  Newton — kurbo alone was off by up to ~130 mp on a 650 pt curve) and
  stops at the other handle. Release emits one `Indents` step; an
  unchanged drag emits nothing.
- **The path stays editable**: the shape editor treats a selected story
  on a path as its path (`edited_paths`), so its nodes show and drag as
  any path's, and the text follows.

Tests: `xarast-app/tests/text_path_edit.rs` (6: fit and remove one step
each, digest-exact undo, the journal replays the fit, fit then remove
renders as before, the path's attributes unchanged; past 90° reversed and
back; a quick shape becomes the path, wrong selections record nothing;
reverse on `TextCurve.xar`: every first-line character at length − d on
the other side, flags START_LEFT ⇄ END_RIGHT; an indent drag and the
clamp; the shape editor on a story's path), `text_path_edit` unit tests
(the split), `xarast-text` `path::tests` (projection).

**Dead end:** comparing distances along the path in **document** space on
`TextCurve.xar`: its first story's matrix carries a shear (c ≈ 0.39), so
document lengths are not story lengths; measure in story space.

### Base SVG along the path (T9.5.6, as built, XARA-T-0252)

**Decision: each character is placed and turned, no `<textPath>`.** The
SVG text placer (`xarast-app/src/svg_text.rs`) lays a story on a path out
exactly as the walker does (`path_fit` → `PathFit::story_mode`) and, when
`PathFit::is_plain` (not reflected, shear tangent ≤ `PLAIN_SHEAR` = 0.005),
reports per character item its glyph origin carried by
`PathFit::cluster_transform` and the turn of that transform
(`StoryPlacement { along_path: true, chars, rotations }`). The writer adds
a `rotate` list to each run next to `x` / `y` (`research/06 §6.7.1` rules
0 and 6). Same output in `.xarast` and SVG export (no dialect branch).
Glyph *origins* are placed, so kerns, tracking, justification, wrap on a
closed path, the straight extension past an open path's ends, the
parallel copies for later lines and baseline shifts all come out exact;
only the renderer's own glyph shapes differ.

**Dead end: `<textPath>`** (built, measured, dropped the same day). A
`<textPath href>` per line over a derived path in a `<defs>` (moved to the
line's baseline, extended or lapped so no glyph falls off) with per-glyph
`x` distances along it:

- **resvg 0.45** follows the path only until the first child *element* of
  the `<textPath>` closes (`usvg` resets its text flow to linear after
  every element child): a line `<tspan>` holding several run `<tspan>`s,
  or a line starting with an empty kern-only run, came out straight.
  Working around it needs one `<textPath>` per run, which Inkscape cannot
  draw:
- **Inkscape 1.2**: it ignores `x` lists inside `<textPath>` (glyphs laid
  end to end with its own advances) and draws nothing sensible when a
  `<text>` has several `<textPath>` children — every multi-line or
  multi-run story on `TextCurve.xar` vanished. It also follows only
  `xlink:href`, not `href`.
- Measured on `TextCurve.xar` (SVG export vs our PNG, resvg): straight
  16.27 → one `<textPath>` per line 8.01 → one per run 3.47 → per-character
  `rotate` **3.06**; Inkscape and resvg both draw the per-character version
  on the curves. It needs no reader change (the reader ignores `rotate`,
  like `x` / `y`), no derived `<defs>`, no new structure.

Kept limitation: reflected or sheared characters cannot be said per
character in SVG (a mirror or a slant is not a rotation), so those stories
stay on straight lines and count in `svg::Stats::text_on_path` (none in
the corpus; `TextCurve.xar`'s one story with a shear has 0.11°, under
`PLAIN_SHEAR`). The app's own save places text too since XARA-T-0259
(`SaveJob` passes `svg_text::placer()`, on the save thread;
`xarast-format.md`, "App save places text"), so File › Save, Save As,
autosave, `xarast-cli convert` and SVG export all write the same base
SVG; only the emergency snapshot on a signal skips the placer.

Evidence: `xarast-text` `path::tests::a_fit_is_plain_unless_characters_are_mirrored_or_visibly_sheared`;
`xarast-format` `tests/svg_text.rs`
`text_on_a_path_is_placed_and_turned_per_character_and_reads_back_unchanged`
(read back, normal form, byte-identical re-save, interchange, the
straight fallback counted); `xarast-app` `tests/svg_text_path.rs` (up a
vertical path: origins on it, turned −90°; reflected stays straight);
`tests/xarast_roundtrip.rs` now saves with the placer and checks the
re-save bytes (59/59 render and bytes); corpus export-check TextCurve svg
3.06 (limit removed).

## Invariants that must not be broken

1. **Nothing outside `xarast-text` names a parley, fontique, skrifa, subsetter
   or brotli type.**
   The public API uses our own types (`FaceData` wraps the blob).
2. **Positions are accumulated in millipoints**, never in `f32` across a
   line. Only per-glyph values come from `f32`.
3. **Tests never see system fonts** unless `XARAST_SYSTEM_FONT_TESTS=1`.
   Deterministic tests use `FontDb::new_isolated()` and the pinned set in
   `crates/xarast-text/tests/fonts/` (provenance and SHA-256 in
   `PROVENANCE.md`). Golden renders in W9.7 must do the same: a render that
   depends on the CI machine's fonts is not a test.
4. **Byte offsets are story-global** in every input and output (`StyleRange`,
   `ManualKern`, `PlacedGlyph::cluster`, `LaidCluster::range`,
   `LaidLine::logical_range`), and every cluster boundary is a char
   boundary.
5. **A grapheme longer than 64 characters is truncated for shaping, never
   passed to parley whole** (see above).
6. **`LaidCluster`s are in logical order; glyph runs in visual order**, left
   to right.
7. **One embedding layer.** Only `xarast_text::embed` reads `fsType` and
   subsets; every writer (`.xarast`, SVG, PDF) goes through it, so a face
   refused in one format is refused in all. Subsets are a function of the
   face bytes and the glyph or character set only (byte-identical
   re-saves depend on it).
8. **In an `xarast-app` test binary, every test installs the pinned fonts
   (`fonts::set_shared`) before it opens a session or touches anything
   that calls `fonts::shared()`** (XARA-T-0294). `shared()` is a
   process-wide `OnceLock`: the first caller wins, and with
   `XARAST_FONT_DIR` unset it picks the *system* fonts. Opening a session
   already calls it (`Session::adopt` fits the view to the page, and
   `viewport.rs` measures text stories with `fonts::document`). One test
   installing the pinned set while its siblings only opened sessions made
   the winner depend on scheduling: `text_path_caret`'s session caret
   drifted ≈ 1,300 mp (system Noto Sans vs the pinned subset) from a caret
   map built with the pinned fonts, 158/200 runs under 48 CPU hogs, 0/60
   idle. The pattern that holds: one helper per binary that caches the
   pinned `Arc` in a `OnceLock`, calls `set_shared` and **asserts**
   `Arc::ptr_eq(&shared(), &pinned)`, called first by every test (see
   `tests/text_path_caret.rs::fonts`). Ignoring `set_shared`'s `bool` hides
   exactly this failure. Not a production race: the shell uses `shared()`
   everywhere and it is fixed once per process.

## Test fonts (pinned set)

`crates/xarast-text/tests/fonts/`, ~200 KB: Noto Sans Regular/Bold/Italic
subsets, Noto Sans Hebrew, Noto Sans Arabic, Noto Sans CJK JP (CFF), all
SIL OFL 1.1 from Debian `fonts-noto-core` 20201225-2 and `fonts-noto-cjk`
1:20230817+repack1-3, no Reserved Font Name (checked in name IDs 0/13/14
and the Debian copyright files), licence text in `OFL.txt`; plus
`XarastTestVariable.ttf`, a synthetic `wght` font we drew
(`make_variable.py`, MIT OR Apache-2.0). `make_subsets.sh` regenerates the
subsets (fontTools 4.65.0). Golden glyph ids in `tests/shaping.rs` are
pinned to these bytes.

## Measurements

Reference machine (`perf.md`), `cargo bench -p xarast-text --bench layout`,
pinned fonts, load ≈ 8:

| Budget | Target | Measured |
|---|---|---|
| Shape + lay out ~1 000 glyphs, justified column | ≤ 8 ms | **0.22 ms** |
| Shape + lay out 10 000 characters (20 paragraphs, 130 lines, Latin + Hebrew, alternating weights) | ≤ 60 ms | **2.28 ms** |
| Glyph outline, cached | ≤ 500 ns | **24 ns** |
| System font enumeration (fontconfig, 2 202 families) | ≤ 300 ms | **40 ms** (opt-in test) |

Binary size (release, `lto = "thin"`, stripped): the text stack costs
**≈ 1.8 MB** without the ICU4X dictionaries and **≈ 5.6 MB** with
`complex-scripts` (the Thai/Lao/Khmer/Myanmar dictionaries are ≈ 3.8 MB of
it), measured on a probe binary that enumerates, lays out and extracts
outlines. `xarast` itself declares `xarast-text` (through `xarast-app`) but
uses none of it yet: +1.2 KB today. The cost arrives when the app first
calls `Shaper`.

**Binary size, shipped (round 2, 2026-09-23).** Stripped release `xarast`
(`lto = "thin"`), before the walker called `Shaper` (1fa7fef) → after:
**21 981 784 → 27 689 320 bytes (+5 707 536, +5.44 MiB, +26 %)** with the
default `complex-scripts`; **23 897 192 (+1 915 408, +1.83 MiB)** without
it. The ICU4X Thai/Lao/Khmer/Myanmar dictionaries are therefore
**3 792 128 bytes (3.62 MiB, 14 % of the binary)**. Recommendation:
**keep** `complex-scripts` — without it a Thai, Lao, Khmer or Burmese
column (no spaces between words) can only break by emergency, which is a
correctness loss in a text tool; if the AppImage size budget bites, load
the dictionaries as data at run time (ICU4X `DataProvider`) rather than
drop them. `ldd` still shows no libwayland, GL, EGL, Vulkan or
fontconfig (fontique dlopens it).

**Start-up (window, `cold_start_ms` to first presented frame, 3 runs,
release, this machine, GPU tiles).** Text-free `Designs/BLUECAR.xar`:
368–427 ms before, 349–421 ms after (noise). Text-heavy
`TextDesigns/Rotated.xar`: 383–390 → 393–403 ms (+≈10 ms: layout and
outlines of 7 stories; enumeration runs in the background from
`fonts::prewarm`). `Designs/GardenPlan.xar` (text + 1 700 objects):
373–403 → 384–399 ms. The 400 ms cold-start budget (XARA-T-0010) is
unchanged in its status: at the edge with or without text. Headless, the
first story of a process waits for enumeration when nothing prewarmed
(CLI: `AngledText.xar` first walk 48 ms, of which ≈ 40 ms fontconfig).

## Dead ends (do not retry)

- ~~Changing the model's default font size to 16 pt~~ — done with
  XARA-T-0172 once the `.xarast` writer/reader stopped using 12 000 mp
  fallbacks of their own (they had broken the normal-form round trip on
  GardenPlan).
- **A `HeadlessOptions::fonts` field**: the struct is `Copy + PartialEq`;
  use `headless::render_with_fonts`.
- **Emitting a text record's attributes after its characters** (the
  structural importer's choice): the model scopes attributes to the
  *following* siblings.

- **Parley's line breaking and `Alignment::Justify`** for Xara-compatible
  layout: CSS semantics, `f32` positions, justification on spaces only.
- **`Cluster::is_word_boundary()` as a line-break opportunity**: it is true
  for word boundaries too.
- **Relying on `Cluster::is_ligature_start` order**: in RTL runs the start
  comes *after* its continuations in `clusters()`.
- **A `fontique` shared collection cloned into the shaper**: `System` fonts
  loaded after the clone are invisible to it (the system handle is per
  clone). Keeping the font context inside the database is simpler.
- **"Roman" as a style suffix**: it strips "Times New Roman" to "Times New".
- **`<textPath>` for text on a path in the base SVG** (T9.5.6): resvg
  loses the path after the first element inside it and Inkscape 1.2
  cannot draw several `<textPath>`s in one `<text>`; per-character `x` /
  `y` / `rotate` works in both (see "Base SVG along the path").
- **A layout-aware pass for text on a path** (spike B: respacing by the
  curvature at each position): it departs from the original by up to 5.8
  advances on the fixtures (above). Do not reopen it for fidelity; only
  as an opt-in style.
- **Chasing a test's text drift in async re-materialisation, gallery
  decode or the `for_document` overlay cache** (XARA-T-0294): the ≈ 1,300 mp
  caret flake was the process-wide `fonts::shared()` being claimed with the
  system fonts by a sibling test (invariant 8). Check that first; forcing
  `fonts::shared()` before `set_shared` reproduces it deterministically.
- **Offsetting each line along each point's own normal** looks nicer but
  is not what the original does (one start-normal vector per line).

## Open TODOs

- Round 2 leftovers: faux **bold** is not synthesised (skew is); underline
  uses fixed proportions, not the font's `post` table; the layer bounds cache (when warm)
  ignores text, so `drawing_rect` misses text once `update_bounds` has run;
  `Viewport::fit_bounds_to` (scroll bounds) ignores text; a per-document
  embedded-font overlay; ~~golden images of `TextDesigns` with pinned
  fonts (W9.7)~~ (done, XARA-T-0260).
- Typing leftovers (T9.4.6): ~~a story emptied by deleting all its text
  stays~~ (done, XARA-T-0237: removed in the deletion's own step); a story
  holding only paragraph breaks is kept when the text is left (the original
  removes it); ~~typed text
  does not pick up attributes chosen while the caret is up~~ (done,
  XARA-T-0225: the caret's pending style); Unicode line/paragraph
  separators (U+2028/9) type as characters, not breaks.
- Text attribute leftovers (XARA-T-0225): text typed at the start of a line
  takes the line's scope, not the following character's own attributes (the
  bar shows the following character's); the bar has no control for
  baseline shift, super/subscript or aspect ratio (the model and
  `SetTextAttr` handle them); line spacing cannot be switched between ratio
  and absolute from the bar; a ruler with every tab removed falls back to
  the ruler a `.xar` line node carries; centre/right/decimal stops can be
  set and now lay out as such (XARA-T-0263; a tab filler character is not
  modelled); the ruler is not
  shown for turned, sheared or mirrored stories or text on a path.
- IME and clipboard leftovers (XARA-T-0224): the candidate window's
  position and preedit behaviour are not yet observed on a real GNOME and
  wlroots session (only headless, XARA-T-0267); a composition at a pending
  caret is not drawn (XARA-T-0268); the system clipboard gets plain text
  only (no HTML/RTF flavour), so styled text survives only inside Xarast,
  and plain text pasted with no caret up is not turned into a new story
  (it is read as SVG) (both XARA-T-0269).
- Convert to shapes leftovers: ~~T9.6.5 outline fallback for export~~
  (done for PDF and SVG export, XARA-T-0245); conformance profile C
  (outlines duplicated in `.xarast`) is not built — there is no profile C
  writer yet.
- Font embedding leftovers: ~~the `.xarast` reader does not register
  the embedded subsets~~ (done, XARA-T-0276, "Embedded fonts on read");
  ~~the `glyf` WOFF2 transform is not applied~~ (done, XARA-T-0276); a
  web font has no `GSUB`/`GPOS`, so a browser draws ligatures and Arabic
  joining from the characters (positions stay exact) and **Xarast itself
  lays out a document face without them** (kerning, ligatures, joining
  lost where the face is missing); `@font-face` declares the face's own
  weight/style, so a browser may synthesise bold where Xarast does not;
  a substituted family's embedded face is used only when the reader's
  ladder reaches the same family.
- Base SVG leftovers (T9.5.6): reflected or sheared text on a path stays
  straight (an SVG `transform` per character would need one element per
  character); ~~the GUI save passes no text placer~~ (done,
  XARA-T-0259).

## The `.xarast` text writer (XARA-T-0172, done)

- The profile carries a story exactly (`research/06 §6.7.1`,
  `docs/memory/xarast-format.md` "Text"): runs of items with every text
  attribute resolved and twinned, kerns / breaks / non-XML characters as
  elements in place. A reload resolves every character as before, so the
  walker lays it out and draws it identically: 59/59 corpus files render
  pixel-identical after a round trip with the pinned fonts.
- **For browsers** the writer asks a `TextPlacer`; `xarast-app`'s
  `svg_text::SvgTextPlacer` runs the same bridge and shaper as the walker
  (`text::story_input`, `layout_text`) and reports each character item's
  cluster box left edge on its baseline (`PlacedGlyph::y` of the
  cluster's first glyph; the line baseline when it has none). The app's
  save path (`save::SaveJob`) passes `svg_text::placer()` in
  `SvgOptions::text`, as `xarast-cli convert` does (XARA-T-0259; bytes
  pinned equal over the corpus by `xarast-cli` `tests/app_save.rs`).
- An empty `TextLine` resolves its line attributes from the state *after*
  its scope closes (`StoryText::end_line`), so its own attributes never
  matter; the format writes and reads that outer state at story level.

- W9.1: T9.1.6 background enumeration (the API is ready; the app must call
  `load_system_fonts` on its I/O thread), T9.1.7 gallery, T9.1.8 Windows and
  macOS smoke tests (T9.1.7's gallery would replace the infobar's plain
  family list, which has no previews). A per-document embedded-font overlay: today an embedded
  face registered in a shared `FontDb` is visible to every document.
- W9.3: ~~T9.3.8 centre/right/decimal tabs~~ (done, XARA-T-0263), T9.3.10
  features/variations are plumbed per run but untested beyond `kern`/`liga`/
  `wght`, T9.3.11 layout cache (the text tool keeps its own per-epoch
  cache of `CaretMap`s; the walker another).
- W9.4 leftovers: typing and grapheme-aware deletion with undo per burst
  (T9.4.6, creates the pending story), IME preedit and the IME caret area
  from the text caret (T9.4.7), clipboard (T9.4.8); ~~text infobar and
  OpenType panel (T9.4.9), the interactive ruler (T9.4.10)~~ (done,
  XARA-T-0225); Ctrl+Up/Down
  by paragraph (they move by line now); the pending caret's height uses
  0.8/0.2 of the current size, not the face's metrics.
- Shaping across a soft line break is not redone: an Arabic word split by
  an emergency break keeps its joined forms. Reshape the two halves if it
  ever matters (only emergency breaks can split a word).
- Which vertical metrics the original used for `FontAscent`/`FontDescent`
  (hhea, typo or win); we use skrifa's (typo when `USE_TYPO_METRICS`, else
  hhea). Check against `TAG_TEXT_LINE_INFO` in the corpus.
- Report the parley `u8` cluster-length overflow upstream.
- `FontMetrics::kern_pair` shapes the pair twice; fine for measuring, not
  for a hot loop.
