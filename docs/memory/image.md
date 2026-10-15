# image

Memory note for **`xarast-image`**: bitmap resources, safe decoding, EXIF,
the legacy `.xar` bitmap wrappings. Phase 10
(`docs/phases/phase-10-bitmaps-and-photo.md`, W10.1/W10.2; story
XARA-US-0051). The phase document calls this note `images.md`; the crate is
`xarast-image`, so the note is `image.md`.

## Current state

Round 1 (2026-09-23) built the decoding half of W10.1/W10.2 as a **leaf
crate**: it depends on `image`, `blake3`, `flate2` and `thiserror` only — not
on `xarast-doc` nor `xarast-xar`, which it reaches as dev-dependencies for
the corpus test.

- `BitmapInfo` (with `ColourSpace`), `BitmapData` (premultiplied RGBA8),
  `OriginalEncoded`, `BitmapResource`, `ImageFormat`.
- `probe` / `probe_as` (header only), `decode` / `decode_as` (sniff, guard,
  decode, orient), `decode_on_worker` (hard wall clock),
  `to_working_space` (the colour-space choke point, a no-op until phase 15).
- `xar::decode_xar_bitmap(tag, bytes, palette, limits)`: the `.xar`
  wrappings; `xar::dib_to_bmp`, `xar::snap_to_palette`.
- Formats: PNG (8/16-bit, grey, palette, APNG first frame), JPEG (baseline
  and progressive, via zune-jpeg), WebP (lossy, lossless, animated first
  frame), GIF (first frame), TIFF, BMP, headerless DIB, PNM.
- **Every bitmap in the 59-file corpus decodes** (table below).
- **The walker decodes and renders them** (XARA-T-0129, 2026-09-23):
  `images_pending == 0` on every corpus file. See "Walker integration
  (as built)".
- **Tag-68 PNG alpha is transparency** (2026-09-23): `decode_xar_bitmap(68)`
  inverts it and `xar::normalise_xar_png` rewrites such a file as a
  standard PNG, which the importer does. See "The `.xar` wrappings".
- The bitmap gallery, File › Import…, background imports and file-list
  paste (XARA-US-0055) — see "Bitmap gallery, import UX and the
  colour-space slot".
- Non-destructive photo adjustments (XARA-US-0054): see "Photo
  adjustments".
- Not yet: `xarast-doc` still has its own `BitmapResource` (SHA-256 over
  pixels + original; XARA-T-0156). Encoders (T10.2.5) are a later
  workstream. The pyramid (XARA-US-0052) and the pixel
  budget (XARA-US-0053) were built in `xarast-render`; see below.

### Corpus census (all 59 files, `tests/corpus.rs`)

`XARAST_XAR_CORPUS=… cargo test -p xarast-image --test corpus -- --nocapture`

| Record kind [sniffed content] | Records | Decoded | Failed | Mpx | With alpha | Decode ms (test profile) |
|---|---|---|---|---|---|---|
| 61 `PREVIEWBITMAP_GIF` [gif] | 58 | 58 | 0 | 0.65 | 0 | 2.7 |
| 67 `DEFINEBITMAP_JPEG` [jpeg] | 4 | 4 | 0 | 0.71 | 0 | 3.8 |
| 68 `DEFINEBITMAP_PNG` [png] | 32 | 32 | 0 | 3.81 | 22 | 15.7 |
| 71 `DEFINEBITMAP_JPEG8BPP` [jpeg] | 8 | 8 | 0 | 2.45 | 0 | 99 (≈ 20 decode + palette snap) |

24 of the 59 files carry bitmap definitions (44 records; the importer
deduplicates them to 38 resources). No tag 60, 62–66 or 69 occurs, so the
headerless-DIB and BMPZIP paths are exercised only by synthetic tests. The
test also asserts that `probe` and `decode` agree on dimensions and that
every output pixel is premultiplied (channel ≤ alpha).

## Decisions taken (and why)

- **`image` 0.25.10 as the façade, default features off**, with exactly
  `png`, `jpeg`, `webp`, `gif`, `tiff`, `bmp`, `pnm` (`research/05 §6`).
  JPEG goes through `image`'s zune-jpeg 0.5 backend — the decoder
  `research/05` picks — so zune is not declared twice. `image` was already in
  the graph via `arboard`; enabling formats unifies into that build too.
  New crates in the graph: `gif` 0.14 (a second `gif`; resvg in `xtask` pins
  0.13), `image-webp`, `zune-jpeg`/`zune-core` 0.5, `color_quant`, `weezl`,
  `fax`, `quick-error`. All MIT / Apache-2.0 / Zlib / BSD / Unlicense
  families; `cargo deny` passes with no new exception.
- **No `kamadak-exif`.** The decoders already return the raw EXIF block and
  `image::metadata::Orientation::from_exif_chunk` parses the one tag import
  needs. The raw block is retained in `DecodedImage::exif` for re-emission.
  Revisit only if phase 11 needs to *edit* EXIF.
- **No `jpeg-decoder` fallback.** zune decoded every corpus JPEG, and a
  second JPEG decoder doubles the attack surface. Add it only on a real file
  zune rejects.
- **Output is premultiplied RGBA8** in non-linear sRGB, like the renderer's
  surfaces. `premultiply` is exactly `round(c·a/255)`; `unpremultiply`
  returns the straight value that premultiplies back to the same byte (both
  exhaustively tested). `BitmapData::to_straight_rgba8` exists because
  `xarast_render::ImageRef` takes **straight** RGBA today; it is exact for
  opaque pixels.
- **Content hash = BLAKE3** over a domain tag, width, height and the
  premultiplied, orientation-normalised pixels
  (`BitmapData::content_hash`). The same picture in PNG and TIFF, or stored
  with any of the 8 EXIF orientations, hashes the same (tested).
- **The JPEG probe does not use `image`**: its JPEG decoder copies the whole
  file before reading a header (≈ 490 µs on a 7.5 MB file, over the 200 µs
  budget). `probe` walks the segments before the first scan itself (SOF
  dimensions, JFIF density, APP1 EXIF, APP2 ICC chunks): 67 ns.
- **Resolution**: PNG `pHYs`, JPEG JFIF density, BMP/DIB pels-per-metre;
  otherwise `DEFAULT_DPI` = 96. `recommended_width` = width × 72 000 / dpi
  millipoints. TIFF/WebP/GIF resolution is not read yet.
- **Colour space**: embedded ICC → `Icc { hash }` (BLAKE3 of the profile,
  bytes in `DecodedImage::icc`); PNG `sRGB` → `Srgb`; grey PNG with `gAMA` →
  `Grey { gamma }`; else `AssumedSrgb`. Anything but the two sRGB variants
  adds `DecodeWarning::ColourSpaceNotConverted` from `to_working_space`, the
  **single** place phase 15 converts.
- **`BitmapInfo::depth` is the source depth** (PNG IHDR bits × channels,
  JPEG components × 8, DIB `biBitCount`, GIF 8), not the stored 32 bpp.

## Decode limits as shipped (`DecodeLimits::default()`)

| Limit | Default | Error | Overridable |
|---|---|---|---|
| `max_dimension` | 65 535 | `TooLarge { limit: Dimension }` | yes |
| `max_pixels` | 256 Mi px | `TooLarge { limit: Pixels }` | yes |
| `max_decoded_bytes` (w·h·4) | 1 GiB | `TooLarge { limit: DecodedBytes }` | yes |
| `refuse_ratio` (native decoded ÷ encoded) | 20 000 | `SuspiciousRatio` | **never** |
| `warn_ratio` | 2 000 | `DecodeWarning::HighRatio` | — |
| `ratio_floor_bytes` | 16 MiB | ratio ignored below it | — |
| `max_jpeg_scans` (SOS count) | 128 | `ExcessiveWork` | **never** |
| `max_alloc_factor` | 1.25 × native + 1 MiB, via `image::Limits::max_alloc` | `AllocationRefused` | **never** |
| `max_duration` | 20 s | `Timeout` | **never** |
| `max_frames` | 1 | first frame only | — |

`DecodeLimits::tight()` (fuzzing, untrusted previews): 4096², 16 Mpx,
64 MiB, 2 s.

Order, which is the security property: sniff → header → size check →
ratio → JPEG scan count → set the allocation ceiling → allocate
(`try_reserve_exact`) → decode → deadline check → convert → orient.

- **Ratio uses the decoder's native size** (`total_bytes()`), not the RGBA
  output: a 1-bit PNG legitimately expands 32× on conversion, and would
  otherwise trip the ratio at ~1 000:1 deflate.
- **Wall clock, two layers.** Cooperative: every decoder reads through a
  `DeadlineReader` that fails with `TimedOut` after the deadline, and
  `decode_as` turns *any* result that arrives late into `Timeout` (a result
  is never returned half-trusted). Hard: `decode_on_worker` runs the decode
  on a thread and returns `Timeout` when `recv_timeout` expires; the
  abandoned thread finishes under the same memory limits. JPEG decodes in one
  call after reading everything, so for JPEG the scan cap is the real time
  guard.
- `image`'s own dimension limit is **off** at header time so that
  `check_size` can report the declared dimensions in `TooLarge`.

## Bomb fixtures (`tests/bombs.rs`; this list only grows)

All generated in the test, none on disk. Each is refused in < 1 s (measured
µs), and the whole suite grows peak RSS by < 64 MiB (measured 32 MiB, most
of it the BMPZIP inflate) — measured by resetting `VmHWM` through
`/proc/self/clear_refs` (Linux; skipped elsewhere). Tests in that file take a
mutex so their RSS figures are not polluted by each other.

| Fixture | Targets | Refused as |
|---|---|---|
| PNG declaring 65 535² | declared-size bomb | `TooLarge(Pixels)` |
| PNG 16 000² grey with a 5 KB IDAT | zlib/declared-size ratio (238 805:1) | `SuspiciousRatio` |
| JPEG with 5 000 empty scans | progressive time bomb | `ExcessiveWork` |
| GIF with a 65 535² logical screen | huge screen | `TooLarge(Pixels)` |
| WebP whose RIFF and VP8L headers disagree | inconsistent header | `Corrupt` |
| WebP VP8X declaring 16 777 216² | huge canvas | `Corrupt` (image-webp refuses) |
| TIFF declaring 65 535² | huge strips | `TooLarge(Pixels)` |
| BMP declaring 60 000² at 32 bpp | huge DIB | `TooLarge(Pixels)` |
| PPM declaring 4 000 000 000 × 1 | huge dimension | `TooLarge(Dimension)` |
| Truncated PNG | early EOF | `Corrupt` |
| BMPZIP inflating 64 MiB under a 16 MiB limit | inflate bomb | `SuspiciousRatio` |

Plus: a zero `max_duration` gives `Timeout`, never pixels; `decode_on_worker`
returns `Timeout` within 500 ms on a 1 ms ceiling; raising `max_pixels` turns
a `TooLarge` into a successful decode ("import anyway").

## The `.xar` wrappings (`xar.rs`; facts in `research/01 §4.5`)

- Sniff first: a recognisable file decodes as itself whatever the tag says.
- Tag 65: headerless DIB → synthesise `BITMAPFILEHEADER` (`dib_to_bmp`).
- Tag 69: zlib or raw DEFLATE, inflated with a cap of
  `max_decoded_bytes + 1 MiB`, then sniff or DIB.
- Tag 71: reconstruct only when the result is an **opaque JPEG** and the
  palette has 1–256 entries (as the original); nearest squared-RGB entry,
  lowest index on ties, no dithering, a 64 Ki-entry exact-key cache. A
  cache miss searches only its colour's cell of a `16^3` grid: each cell
  lists, built on first use, the entries whose distance to the cell box is
  ≤ the least worst-case distance of any entry to the box, so the answer
  (ties included) is the brute-force one (XARA-T-0287). Sets
  `depth = 8`, `palette_entries = n`.
- Tag 68: a PNG of colour type 4 (grey + alpha) or 6 (RGBA) stores
  **transparency** in its alpha channel, 0 = opaque (facts and
  `file:line` in `research/01 §4.5`). `decode_xar_bitmap(68, …)` flips the
  alpha samples in the decoder's native buffer (bitwise NOT, so 8 and 16
  bit alike) **before** premultiplying — flipping after would have
  premultiplied the colour of every really-opaque pixel by zero.
  `normalise_xar_png` does the same flip and re-encodes with `image`'s PNG
  encoder at the same depth and colour type (lossless, never interlaced),
  then splices the original's ancillary chunks (`pHYs`, `iCCP`, `sRGB`,
  text…) around the new `IHDR`/`IDAT` in their original order. Palette
  PNGs are left alone: the original writes ≤ 8 bpp with a single
  transparent index, which means what it says. `png_alpha_is_transparency`
  is the IHDR test (colour type byte at offset 25).
- 60–64 (previews): plain files.

## Procedural generators (`procedural.rs`, XARA-T-0336)

- `clouds(&CloudsParams)` and `plasma(&PlasmaParams)` return grey tiles,
  **row 0 at the fill's start point** (bottom-up); `Procedural` is both
  the input and the cache key; `FillTile` turns a document fill (floats,
  axis lengths, dpi) into one, choosing the clouds size by the original's
  rule (power of two, 4..=256, from dpi and the axes; plasma is always
  256).
- **Clouds** is the original's integer algorithm restated in
  `research/03 §2.8.1` (33-bit shift register, corner stretch, edge order
  bottom/right/top/left, `noise − attraction + mean`, clamp to 0..65535,
  pixel = low byte of `!(p >> 8)`). **Plasma** is the quadratic B-spline
  over a 50 × 50 table; the table is the Microsoft C runtime's `rand()`
  (×214013 + 2531011, bits 16..30) — the original used the platform's
  `rand()`, so its own plasma varied by platform; matching Windows is
  unverified.
- **Frozen (H6)**: `tests/procedural.rs` hashes eight tiles (three seeds
  per generator plus tileable, gravity and squash variants) and checks
  four threads agree. A change to the pixels must bump
  `CLOUDS_GENERATOR` / `PLASMA_GENERATOR` (written as `xarast:generator`)
  and keep the old one, never edit a hash.
- Integer edge cases the source left to the compiler are defined:
  rotations by 0 are the identity, a shift past 31 is 0, conversions
  saturate, so extreme `FIXED16` values cannot panic (unit test).

## Invariants that must not be broken

1. No pixel buffer is allocated before `check_size`, the ratio and the scan
   count have passed. The two big allocations use `try_reserve_exact`.
2. A `DecodeError` means no pixels; a late result is `Timeout`.
3. Output: `width·height·4` bytes, every channel ≤ alpha, dimensions after
   orientation. Asserted by `fuzz_image_decode` and the corpus test.
4. `probe(b).info == decode(b).info` except `has_alpha` (probe reports the
   layout's alpha channel, decode whether any pixel is translucent).
5. Only `TooLarge` is overridable.
6. Only `to_working_space` may convert colour spaces.

## Fuzzing

`fuzz/fuzz_targets/fuzz_image_decode.rs`: first byte picks the façade
(`probe` + `decode`) or the `.xar` wrapper (tag selector, palette length,
palette, image). `DecodeLimits::tight()`. Seeds come from
`cargo run -p xarast-image --example fuzz_seeds -- <dir>` (12 synthetic
seeds: every format plus DIB, BMPZIP and JPEG8BPP); CI generates them, none
are committed. Nightly in `.github/workflows/fuzz.yml`.

- 2026-09-23, first run: **one finding** after 1.14 M executions — a
  translucent WebP under tag 71 was palette-snapped, leaving colour above
  alpha. Fixed (reconstruct only an opaque JPEG; `snap_to_palette` now
  matches on the straight colour and premultiplies back) with the regression
  test `jpeg8bpp_leaves_a_translucent_non_jpeg_alone`.
- Second run after the fix, 601 s on the grown corpus: **4 694 765
  executions, clean** (7 811 exec/s, peak RSS 658 MB with
  `-malloc_limit_mb=1024`, slowest unit < 1 s, 14 523 new units).

## Walker integration (as built, XARA-T-0129)

`SceneWalker::register_images` (`xarast-app/src/walker.rs`), run once per
frame before the walk:

1. A resource whose `pixels` are `w·h·4` bytes is registered as is (the
   native path). One with empty `pixels` and an `original` is decoded.
2. Decoding (`decode_resource`), `DecodeLimits::default()`:
   `Jpeg` **with a palette** → `decode_xar_bitmap(71, …, palette)`;
   `Png | Jpeg | Gif` → `decode`; `Bmp` → `decode_xar_bitmap(65)`;
   `Unknown` (the importer's BMPZIP) → `decode_xar_bitmap(69)`. PNGs need
   no tag-68 handling here: the importer already normalised them.
3. Registered as `ImageRef::new(d.width, d.height, d.to_straight_rgba8())`
   — straight alpha, decoded dimensions (the importer leaves `info` zeroed).
4. All pending resources of a frame decode together on scoped threads
   (`decode_all`, at most `available_parallelism`, work-stealing by an
   atomic index), results applied in input order so registration — and
   the scene — stay deterministic. A panicking decoder is a failure.
5. Failures go into `failed: HashSet<BitmapId>` (cleared by `reset`) and
   are never retried by that walker. Objects referring to them count in
   `WalkStats::images_failed`; objects whose bitmap has neither pixels nor
   bytes count in `images_pending`. A bitmap **transparency** whose image
   is missing counts too (it composites opaque: a shadow turns into a
   black box). `is_complete` requires both to be 0.
6. Cost (release, this machine): the per-file first-walk increase is
   ≤ 38 ms (leafgirl, a 649×430 JPEG8BPP snap; scope3 27 ms); every other
   bitmap file ≤ 8 ms. Since the cell-grid snap (XARA-T-0287) the worst
   first walk is Groucho2's 18 ms (`perf.md`). Open-to-first-paint in the real window is unchanged
   within noise (≈ 330–370 ms for the bitmap files, both before and after;
   `perf.md`). The first decode of a bitmap is still on the walk (the
   main thread); what XARA-T-0281 moved off the render thread is
   bringing an *evicted* base back (below, "Pixel memory budget").
7. **One decode per document** (XARA-T-0281): a `Session` owns a
   `DecodedImages` (`xarast-app/src/decoded.rs`) shared by every walker
   made for it — its own, `build_scene`, the CLI export, headless renders
   and the save thread's thumbnail. A walker looks each bitmap up there
   before decoding and files what it decodes (failures too, so a bad
   bitmap is tried once per document). Key: the addresses of the
   resource's `pixels` and `original` `Arc`s (held by the entry, so they
   cannot be reused), its declared size and the pixel budget. Resources
   are copy-on-write, so a changed bitmap misses. The bitmap gallery's
   thumbnail thread files there too (XARA-T-0293, below). A walker made with
   plain `SceneWalker::new()` (tests, corpus tools) still decodes for
   itself; `SceneWalker::with_decoded_images` opts in. Numbers in
   `perf.md`.

Bitmap **fills** render too, and tile by the fill-mapping attribute
(XARA-T-0054, `paint.rs::bitmap_repeat`): the fill's own `tiling` wins
when set (a `.xarast` fill may carry one), otherwise the attribute, and
unset means repeat — the original's default. `Fill Types simple.xar`'s
bitmap row shows single, repeating and mirrored tiles, contone and
duotone.

`.xarast` round trip: bitmaps survive — the package stores the
(normalised) original bytes and, for tag 71, the snap palette as a blob
(`xarast:palette`, XARA-T-0154). The render round trip is pixel-identical
for 59/59; the writer gaps bitmaps first exposed (palette, contone,
fill mapping, transparency image) are all closed. Bitmap fills and
transparencies used to be drawn upside down (XARA-T-0171, found against
resvg): a fill's start point is the image's **bottom**-left corner, while
the renderer samples decoded (top-down) rows from `v = 0`.
`paint::bitmap_frame` now starts the frame at the top edge; facts,
`file:line` and SSIM in `render.md`, "Bitmap fill orientation". Contone
and duotone tiles and the mirrored mapping are upright too.

### Resampling (XARA-US-0052, 2026-09-24)

How a decoded bitmap is *sampled* is the renderer's
(`xarast-render/src/resample.rs`; decisions, harness numbers and the
kernel choice in `render.md`, "Resampling quality"). What matters on this
side:

- `ImageRef` keeps **straight** RGBA8, as registered by the walker; the
  sampler premultiplies (and linearises, for minification) itself, so there is no reason to
  hand it premultiplied data.
- The **mip pyramid** (+⅓ of the image's bytes) is shared by every clone
  of the `ImageRef`. Since XARA-US-0053 the walker builds it on its decode
  threads (`ImageRef::prepare`, right after the decode), so the render
  thread no longer pays 26 ms per 2048² on the first minified frame; an
  image nobody prepared still builds it lazily on first minification.
- Contone (duotone) is applied per texel through a 256-entry table, before
  filtering, as the original does in its sampler; bitmap transparencies
  are filtered like colour fills (`TranspSource::Image` has a `Filter`).
- Not wired: the document's smoothing flag (XARA-T-0273).

### Editing bitmap fills and placing bitmaps (XARA-T-0271, XARA-T-0272)

Decisions in `tools.md` 64–69; what matters on this side:

- **A placed image is decoded once, at placement**
  (`xarast-app/src/place.rs`), under `DecodeLimits::default()`, only to
  refuse what is not an image and to learn its size and resolution. The
  decoded pixels are thrown away: the document keeps the encoded bytes
  (`pixels` empty), and the walker decodes them like any opened file's.
- **Stored bytes**: PNG, JPEG and GIF as they arrived (the formats the
  document's `ImageFormat` and the walker's `decode_resource` know);
  WebP, TIFF, BMP and PNM are converted once to a lossless 8-bit RGBA PNG
  through `xarast_io::png::encode_png`, keeping the resolution in `pHYs`
  (no `xarast-image` encoder exists yet, T10.2.5). A clipboard picture
  (straight RGBA from `arboard`) becomes a PNG with no `pHYs`.
- **Natural size** = `pixels × 72 000 / dpi` per axis
  (`xarast_doc::bitmap_fill::natural_length`, the same rule as
  `BitmapInfo::natural_width`); no resolution means 96 dpi. The document's
  `BitmapInfo` of a placed image is filled in (size, 32 bpp, dpi), unlike
  the `.xar` importer's; `place::bitmap_pixels` falls back to a header
  probe of the bytes when it is empty.
- The resource is added outside the undo history and deduplicated by
  content; `collect_unused` keeps it while a redo step holds its object.
- The object gets the original's default bitmap attributes — no line
  colour, no fill colour, zero line width
  (`xarast_doc::default_bitmap_attrs`).
- Not done: EXIF orientation is applied by every decode of the stored
  JPEG (the walker's included), so a rotated JPEG places upright but its
  natural size is read from the oriented layout — consistent, but not
  checked against a real rotated file.

### Bitmap gallery, import UX and the colour-space slot (XARA-US-0055)

Decisions in `tools.md` 70–73 and `ui.md` (bitmap gallery section,
decisions 29 and 42); what matters on this side:

- **Thumbnails** (`xarast-app/src/bitmap_gallery.rs`, T10.7.3): one
  background thread, started on the first request, fed resources — each
  with its document's `DecodedImages` — by channel. Since XARA-T-0293
  the thread has **no decoder of its own**: `DecodedImages::image_for`
  returns the image the view already decoded, or decodes it through the
  walker's `ready_image` (the one dispatch: native pixels, the façade,
  tag 71 with its palette, 65, 69; then the pyramid) under the
  process-wide budget the session's walkers use, and files it — failures
  too. A resource that is neither native nor encoded is not filed, so
  the walker still counts it pending. The thumbnail is then
  box-filtered from the image's **base** (`ImageRef::level(0)`, which
  brings an evicted base back in place; the thread may block, the UI
  never does) over alpha-weighted colour to ≤ 64 px on the longer side,
  never enlarged. A panicking
  decoder is a failed thumbnail. Keyed by the resource's **content hash**
  (`DocumentResources::bitmap_key`, the SHA-256 kept from insertion — no
  re-hashing), shared by every document, **in memory only**: the disk
  cache T10.7.3 asks for is XARA-T-0289, and so is measuring the
  100-thumbnails-in-1.5 s budget.
- **Thumbnails before and after XARA-T-0293 are identical**:
  `tests/gallery_thumbnails.rs` keeps the old dispatch as an oracle and
  compares byte for byte on synthetic bitmaps (native, PNG, corrupt,
  empty) and on all 44 corpus bitmap records, decoded fresh and found
  in the session's cache. `tests/decoded_images.rs` counts decodes:
  the gallery on a walked document decodes nothing (2 hits), and a walk
  after the gallery decoded a new bitmap decodes nothing (3 hits). Not
  chosen: shrinking from a pyramid level instead of the base (cheaper
  for a big photograph, but a different, linear-light filter — the
  thumbnails would change); revisit with the disk cache (XARA-T-0289).
  A gallery-only thumbnail now keeps its decoded image in the cache —
  no extra cost in practice, since the walker registers every bitmap
  of the document, used or not, on its first frame anyway.
- **Background decode** (`import.rs`, T10.7.5): files over 1 MiB are
  read in 256 KiB chunks on a thread and decoded there with
  `place::image_from_bytes` — the same decode, limits and storage rule
  as an inline import. A cancelled job's thread stops at the next chunk
  or finishes its decode under the decode's own deadline; its result is
  dropped.
- **Colour space (W10.8)**: T10.8.1 (sniffing), T10.8.3 (the assumed
  flag) and T10.8.4 (the single `to_working_space` choke point, a no-op
  plus `ColourSpaceNotConverted`) were already in round 1. Every import
  goes through `decode`, so through the choke point. The gallery
  **reports** each bitmap's colour space from a header probe ("sRGB",
  "sRGB (assumed)", "ICC profile (not converted)", "grey, gamma γ"); the
  document's `BitmapInfo` still has no colour-space field. A PNG, JPEG
  or GIF keeps an embedded ICC profile inside its stored bytes, which the
  `.xarast` writer emits verbatim, so nothing is lost; a WebP, TIFF, BMP
  or PNM converted to PNG at placement **loses its profile** (the
  `xarast-io` encoder writes an `sRGB` chunk and no `iCCP`). T10.8.2 —
  profiles as document resources under `resources/profiles/` — is open
  (XARA-T-0291). Replace-bitmap (T10.7.6) is XARA-T-0290.

### Pixel memory budget (XARA-US-0053, 2026-09-24)

The budget itself lives in `xarast-render` (`pixel_budget.rs`,
`spill.rs`; design, proofs and numbers in `render.md`, "Pixel memory
budget", and the proxy rule in `perf.md`) because the levels it budgets
are `ImageRef`'s. This crate is unchanged. The walker's side
(`make_image`, `Encoded` in `xarast-app/src/walker.rs`):

- Every registered image gets a `PixelSource`. **Native pixels**
  (`w·h·4` bytes in the document): a *cheap* source copying the
  document's `Arc<[u8]>`, so an evicted base is dropped and copied back,
  never spilled. **Encoded originals**: an *expensive* source that runs
  the same decode again (`Encoded::decode`: tag 71 with the palette, 65,
  69, or the façade) and checks the dimensions; the budget spills such a
  base on its first eviction and reads the spill file back after that.
  Both re-produce the registered bytes exactly: every decoder here is
  deterministic (tested over the 24 corpus files with bitmaps, byte for
  byte, spilling and re-decoding).
- Native and encoded resources now go through the same worker pool
  (`decode_all`), which also calls `prepare`; results are still applied
  in input order, so registration is deterministic.
- `SceneWalker::with_pixel_budget` registers under a budget other than
  the process-wide one; `headless::render_with_walker` renders with such
  a walker.
- The document's own bytes (`BitmapResource::pixels`, `original`) are
  **not** in the budget; only the renderer's decoded levels are.
- **Nothing on the render thread waits for a decode any more**
  (XARA-T-0281): the interactive renderer draws an evicted image from a
  smaller resident level and a helper thread runs this crate's
  re-decode (the `PixelSource`) or the spill read; the render thread then
  repaints the image's damage (`render.md`, "Drawing without waiting for
  an evicted base"; `app-core.md` decision 41). Export and thumbnails
  still re-produce an evicted base in place, byte for byte.
- The cached `ImageRef`s of `DecodedImages` stay under the budget like
  any other: sharing them between walkers shares their proxies and
  eviction state too (an export at 300 dpi may grow a proxy for the
  session's view — the proxy rule is per image, not per walker).

### Photo adjustments (XARA-US-0054, W10.6, 2026-09-24)

Non-destructive: the resource is the **master** and is never touched; a
bitmap object shows the master put through its chain. Three crates, one
job each:

- **Model — `xarast_doc::photo`.** `BitmapNode::photo_ops: PhotoOps`
  (empty for every `.xar` import). `PhotoOp`: `Crop(PixelRect)` in master
  pixels, `Orient(PhotoOrient { turns, flip })` (mirror left-right, then
  quarter turns clockwise; `then` composes, the dihedral law is tested),
  `Levels { channel: rgb|red|green|blue, in_lo, in_hi, out_lo, out_hi }`,
  `Gamma` (`x^(1/γ)`, clamped to 0.05–8), `Brightness` (`x + b`),
  `Contrast` (`(x − ½)(1 + c) + ½`), `Saturation` (distance from Rec.601
  luma × `1 + s`), `Greyscale`, and `Unknown { kind, raw }`.
  **The chain is a set with one canonical order**, not a free sequence:
  crop → orient → levels R, G, B, then rgb → gamma → brightness →
  contrast → saturation → greyscale. `normalise` sorts into it, keeps the
  **last** value per kind, clamps, drops neutral ops; `hash` (SHA-256 of
  the normalised chain) is therefore independent of the order the user
  set things in (criterion 13's second half, tested). The parameter
  semantics are ours: the research has only the `GBitmap_Set*` names,
  CDraw is closed.
- **Unknown ops** make the chain non-editable: `normalise` leaves it
  exactly as read, `SetPhotoOps` refuses it (`NotPermitted`), rendering
  skips them (`PhotoOps::evaluable`) and the writer re-emits their text.
- **`SetPhotoOps { node, ops, master, label }`**: one undo step
  (`set_kind`); stores the normalised chain. With the master's size it
  moves the object: a crop keeps the kept pixels where they were on the
  page (`place ∘ M_old⁻¹ ∘ M_new`, `M` = `unit_to_master`, the derived
  unit square to master pixels); a change of orientation turns the
  picture *inside* the object (same centre, same edge directions, same
  size per pixel, new width/height). `Session::set_photo_ops` supplies
  the size (`place::bitmap_pixels`) and records no step for a chain that
  normalises to the current one.
- **Pixels — `xarast_image::photo`.** `Recipe { crop, flip, turns, lut,
  mix }` over straight RGBA8, alpha untouched. **LUT fusion (T10.6.4):**
  `fuse` runs each of the 256 levels through every point op in `f64` and
  rounds **once**, per channel; saturation and greyscale mix channels and
  run per pixel after the table. Geometry is pure copies (crop + mirror in
  one pass, then a quarter-turn or 180° pass). Deterministic by
  construction (the budget re-creates evicted derived images from it).
  24 Mpx brightness + contrast + gamma: **66 ms** single-threaded in the
  test profile (budget 200 ms; no rayon needed yet), checked pixel by
  pixel against the fused table (`tests/photo.rs`, criterion 13). Since
  XARA-T-0301 the pixel pass (table + mixes) runs on scoped threads in
  bands of ≥ 128 Ki pixels (`available_parallelism`); every pixel is
  independent, so the bytes are the same (the criterion-13 test covers
  the banded path). The geometry copies stay single-threaded.
- **The translation — `xarast_io::photo`.** `recipe(&PhotoOps)` and
  `bake(w, h, rgba, ops)`: the only place a document chain becomes a
  recipe, used by the walker and the SVG exporter alike, so the screen,
  PNG/PDF and SVG show the same pixels. `baked_pixels_sit_where_the_model_
  says` pins the model's `unit_to_master` to the baked pixels for all 8
  orientations × 3 crops.
- **The walker** (`derived_image`, `derive`): a bitmap object with a
  non-empty evaluable chain paints a **derived image** keyed by
  (master `BitmapId`, chain hash). Evaluated on first paint from the
  registered master's base, registered with an *expensive* `PixelSource`
  that re-evaluates (so the budget spills it on first eviction), pyramid
  built (`prepare`). An identity recipe paints the master itself. Cached
  in the walker and in `DecodedImages` (`get_derived`/`insert_derived`,
  stats `derived`, `derived_hits`, `derived_pruned`), so export,
  thumbnail and `build_scene` walkers reuse the view's evaluation
  (tested: an export walk evaluates nothing). **Pruning:** when the
  document epoch moves, pairs used by no *attached* bitmap object are
  dropped from both caches (history-only chains are re-evaluated on
  undo); the walker's registry slot is parked with a 1 × 1 placeholder
  and reused (`ImageRegistry::replace`), so twenty slider values leave
  the registry at most one slot bigger (tested). Since XARA-T-0304 the
  session's own walker evaluates nothing large on the walk: see "The
  release, off the walk thread" below. Every other walker (export,
  thumbnails, `build_scene`, headless) still evaluates at full resolution
  on its walk, or finds the session's image in `DecodedImages`.
- **Live proxy preview (XARA-T-0301, T10.6.5).** While a photo panel
  slider is dragged, `Preview::photo` carries `(node, chain)` and the
  walker draws that object from a **proxy** (`SceneWalker::proxy_image`):
  the chain evaluated on a reduced level of the registered master
  (`ImageRef::level(k)`, straight sRGB like the base; the pyramid was
  built at decode). `proxy_level` takes the smallest level whose output
  still has as many pixels as the object shows on screen, then smaller
  until the level holds ≤ `PROXY_MAX_PIXELS` (2.1 M, about full HD);
  the crop is scaled onto the level and rounded outwards
  (`scale_recipe`). The parallelogram is unchanged, so a smaller image
  lands in the same place. A proxy is registered unbudgeted-source
  (`None`) in one slot per previewed object, reused frame to frame
  (another slider value replaces the slot's image), and parked with the
  placeholder into `derived_free` the first walk nobody previews it —
  so the commit's full-resolution derived image usually takes that very
  slot. Nothing goes into `DecodedImages` (tests assert its `derived`
  stat stays 0 through a drag). The node's fingerprint mixes the chain
  hash, and `scene_damage` compares the new image by content, so damage
  stays on the object (tested each frame beside a second picture).
  `SceneWalker::photo_proxies` / `Session::photo_proxies` report the
  level and size per frame.
- **Measured** (test profile, this 24-core machine shared with other
  agents; measured then by a timed test, now by the perf gate
  `photo-slider-24mpx`, `xarast-cli bench photo`, see `perf.md`;
  6000 × 4000 native master shown at ≈ 1200 × 800 in a 1280 × 800
  view): proxy level 2 (1500 × 1000). At load average ≈ 10: intent +
  walk with the proxy evaluation **3.3 ms median, 10.5 ms max**; whole
  slider frame with the CPU render of the view **19.1 ms median, 27.9 ms
  p90, 31.0 ms max** over 30 frames. Before the banded pixel pass the
  walk was ≈ 10 ms; with the machine loaded (load ≈ 80–115) the frame
  median reached ≈ 33 ms. The test
  (`a_slider_drag_on_a_24_mpx_photo_draws_proxies`) asserts no clock,
  only that the document is untouched and every frame is a proxy. The
  release used to evaluate the full 24 Mpx on the walk thread, ≈ 60 ms
  evaluation + ≈ 200 ms pyramid (`prepare`) ≈ 264 ms once; see the next
  item.
- **The release, off the walk thread (XARA-T-0304).** The session's
  walker is made `SceneWalker::with_deferred_derived`: a derived image of
  at least `DEFER_MIN_PIXELS` (1 Mi px) is registered with
  `ImageRef::deferred` — nothing resident but a **stand-in**, and a
  source that runs the same evaluation `derive` runs (`derived_source`,
  shared by both, so the bytes are the same). The stand-in is the proxy
  the last slider frame drew of that very chain (still in `proxies`
  during the release walk; its pixels are shared, not copied), so the
  release frame shows exactly what the drag's last frame showed; with no
  such proxy (undo, redo, opening a file, an off-screen commit) the
  chain is evaluated on the level `proxy_level` picks, never below
  level 1. The interactive render thread draws the stand-in
  (`MissingLevels::Substitute`), its helper makes the image **and its
  whole pyramid** off both threads, and the worker repaints the
  picture's damage under `Materialise` (`render.md`, "Deferred images";
  `app-core.md` decision 41). Export, thumbnails and headless renders
  sample under `Materialise`, which makes a pending image in place,
  byte for byte — they never show a stand-in. Images under 1 Mi px
  (≈ 10 ms of evaluation and pyramid) are still evaluated on the walk.
  Measured (`xarast-cli bench photo`, release build, load 33–36): the
  release walk **0.2 ms** (was 223–373 ms at load 8–14), the release
  frame 25–34 ms (the same as a slider frame: the render of the
  picture), exact again 314–366 ms after the release. Gates:
  `photo-release-24mpx`, `photo-release-walk-24mpx`,
  `photo-converge-24mpx` (`perf.md`). Tests:
  `xarast-app/tests/photo_release.rs` (the walk makes nothing; the
  settled frame is a fresh walker's full-resolution picture byte for
  byte, with every repainted rectangle on the photograph; undo and redo
  while the helper works settle on the right picture and no exact frame
  after them shows the stale one) and
  `xarast-render/tests/deferred_images.rs`.
- **Damage:** an adjustment changes one leaf's image id; `scene_damage`
  compares images by content, so only that object repaints and undo gives
  no damage against the frame before (tested with a second picture
  beside it). A reused slot is safe: the render thread holds a resolver
  snapshot per rebuild.
- **Export:** PNG/JPEG/WebP and PDF render the scene, so they show the
  derived image with no extra code (PDF rasterises placed images anyway).
  SVG bakes: `SvgOptions::derived_bitmaps` (`DerivedLinker`) makes a PNG
  of the adjusted pixels (the master's ICC profile travels) and the
  report says `Simplified { "photo adjustments baked into a PNG of the
  adjusted pixels" }`; without a baker the master shows and the report
  says `NotRendered`.
- **`.xarast`:** `<xarast:photo-ops>` inside the `<image>`, `href` = the
  master (`xarast-format.md`). Derived renditions in `resources/derived/`
  (T10.6.6) and a browser-correct base picture are XARA-T-0302; chains on
  bitmap fills are XARA-T-0303.

## Dead ends (do not retry)

- Inverting a tag-68 PNG's alpha **after** `decode` (on premultiplied
  output): every pixel the file marks 0 (= opaque) has already had its
  colour multiplied by zero. The flip must happen on the native buffer.

- Probing JPEG through `image::codecs::jpeg::JpegDecoder`: copies the whole
  input first; 7× over the probe budget on a 24 Mpx photograph.
- Counting JPEG scans in `probe`: linear in the file (≈ 3 ms on 7.5 MB);
  only `decode` counts them.
- A per-pixel full palette search for tag 71: 358 ms on the corpus's
  2.45 Mpx; the exact-key cache brings it to ≈ 80 ms.
- For the tag-71 cache misses (XARA-T-0287; Groucho2's 764×512 photograph,
  125 k misses, snap alone; the JPEG decode itself is ≈ 4 ms more): the
  256-entry scan ≈ 30 ms; a branch-free packed `(distance << 14 | index)`
  minimum over SoA lanes ≈ 22 ms (baseline x86-64 has no `pmulld`);
  entries sorted on the widest channel with a pruned outward walk 17.7 ms
  (JPEG colours sit far from the palette, so little prunes). The cell
  grid: 7.8 ms, of which < 0.5 ms builds the ≤ 363 cells a photograph
  touches (≈ 20 candidates each). A `32^3` grid saves another
  ~1 ms on the largest image and costs more on the small ones.

## Open TODOs

- Reconcile `xarast-doc::BitmapResource` (SHA-256 over pixels + original)
  with this crate's (BLAKE3 over pixels) — T10.1.2, XARA-T-0156. It did not
  block the walker, which never stores decoded pixels in the document.
- Palette PNGs under tag 68 whose `tRNS` has several non-opaque entries:
  the original expands them to RGBA and so reads their alpha inverted too;
  we leave palette PNGs alone. No corpus file has one, and the original's
  writer never produces one.
- Encoders for resource storage (T10.2.5).
- TIFF/WebP/GIF resolution; PNG `iCCP`-vs-`sRGB` precedence when both exist.
- `DecodeLimits::max_frames` is informational: every decoder already takes
  the first frame only.
- Photo adjustments: derived renditions in `.xarast` and a
  browser-correct base picture (XARA-T-0302); chains on bitmap fills
  (XARA-T-0303). (The panel and the live proxy preview: done,
  XARA-T-0301; the release off the walk thread: done, XARA-T-0304.)
- The helper builds a deferred image's pyramid single-threaded
  (≈ 200 of the ≈ 300 ms to the exact frame on 24 Mpx). Banding
  `reduce_level` would shorten the wait for the exact picture; nothing
  blocks on it.
- Bitmap gallery (XARA-US-0055): thumbnails on disk and the thumbnail
  budget (XARA-T-0289); Replace and Save a copy (XARA-T-0290); ICC
  profiles as resources and through the placement PNG conversion
  (XARA-T-0291). (One decode dispatch for the walker and the
  thumbnailer: done, XARA-T-0293.)
