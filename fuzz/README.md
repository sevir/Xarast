# Fuzzing

Eighteen `cargo-fuzz` targets, in their own workspace so that the main one
stays on stable. Six cover the `.xar` importer — a legacy binary format
parser is attack surface, so fuzzing is part of Phase 3 rather than a
follow-up (`docs/phases/phase-03-xar-importer.md` W3.11) — six cover
the geometry, render and document-model layers the importer feeds, four
cover the native `.xarast` container (Phase 6), one the operation
journal (Phase 6) and one image decoding (Phase 10).

| Target | Crate | What it drives |
|---|---|---|
| `fuzz_xar_records` | `xarast-xar` | magic, framing, deflate, CRC trailer |
| `fuzz_xar_tree` | `xarast-xar` | `DOWN`/`UP` nesting, atomic stripping, depth cap |
| `fuzz_xar_decode` | `xarast-xar` | every typed decoder over an arbitrary tree |
| `fuzz_xar_import` | `xarast-xar` | the whole pipeline into `DocumentBuilder` |
| `fuzz_xar_path` | `xarast-xar` | the relative/absolute path codecs and flags |
| `fuzz_xar_colour` | `xarast-xar` | colour records, inherit sentinels, parents |
| `fuzz_path_boolean` | `xarast-geom` | two paths through a `BoolOp` × `FillRule`, and `self_union` |
| `fuzz_svg_path_parse` | `xarast-geom` | SVG path data as text, and the exact round trip |
| `fuzz_hit_test` | `xarast-geom` | `hit_fill`/`hit_stroke` on arbitrary paths, transforms and styles; `HitIndex` edits against a brute-force list |
| `fuzz_display_list` | `xarast-render` | scenes through `DisplayList::build` and the CPU backend |
| `fuzz_ramp` | `xarast-render` | ramp tables, profiles, gradient evaluation |
| `fuzz_doc_builder` | `xarast-doc` | arbitrary build scripts: "valid or nothing" |
| `fuzz_journal_read` | `xarast-doc` | the operation journal's NDJSON reader, its base reader and replay of whatever it accepts: never panic, a refused entry changes nothing |
| `fuzz_xarast_open` | `xarast-format` | `XarastReader::open`, every read path, and a re-save of whatever opens |
| `fuzz_xarast_repair` | `xarast-format` | `repair` over arbitrary bytes (a rebuilt package must open strictly and repair again), `open_recovering`, the partial-XML closer |
| `fuzz_xarast_manifest` | `xarast-format` | the manifest parser and its write–parse fixed point |
| `fuzz_xarast_svg_read` | `xarast-format` | the SVG profile reader on arbitrary text, and the read–save–read fixed point of what it accepts |
| `fuzz_image_decode` | `xarast-image` | `probe` + `decode` over every format with `DecodeLimits::tight()`, and the `.xar` bitmap wrappings |

The six structured targets take their input through `arbitrary`; the
helpers they share are in `fuzz_targets/common.rs`.

```sh
cargo +nightly fuzz run -O fuzz_xar_records -- -max_total_time=600 \
    -timeout=5 -rss_limit_mb=2048 -malloc_limit_mb=1024
cargo +nightly fuzz run -O fuzz_svg_path_parse -- -max_total_time=600 \
    -dict=dicts/svg_path_parse.dict
```

Any other target runs the same way. The `.xarast` targets have no
committed seeds (everything under `corpus/` must be a seed that
`crates/xarast-xar/tests/fuzz_seeds.rs` generates, and that test fails on
anything else). Write them to a scratch directory first; CI writes them to
`corpus/` because it runs no tests afterwards:

```sh
cargo run -p xarast-format --example fuzz_seeds -- /tmp/xarast-seeds
cargo +nightly fuzz run -O fuzz_xarast_open /tmp/grown /tmp/xarast-seeds/fuzz_xarast_open \
    -- -dict=dicts/xarast_open.dict
```

The image seeds are generated the same way, by
`crates/xarast-image/examples/fuzz_seeds.rs`:

```sh
cargo run -p xarast-image --example fuzz_seeds -- /tmp/image-seeds
cargo +nightly fuzz run -O fuzz_image_decode /tmp/grown /tmp/image-seeds/fuzz_image_decode \
    -- -malloc_limit_mb=1024
```

A local run should pass a scratch
directory as the *first* corpus argument, so that what the fuzzer
discovers never lands in `corpus/`:

```sh
cargo +nightly fuzz run -O fuzz_xar_import /tmp/grown corpus/fuzz_xar_import
```

## In CI

`.github/workflows/fuzz.yml` runs every target for five minutes each
night (02:30 UTC), and on demand with a chosen duration. A crash, panic,
OOM or timeout fails that target's job and uploads the reproducer as an
artefact. The corpus each run grows is carried between nights in the
Actions cache, never committed.

## The invariants

1. **Never panic.** No unwrap on untrusted data, no unchecked index, no
   arithmetic overflow in a debug build. `xarast-xar` denies
   `clippy::indexing_slicing`, `unwrap_used`, `expect_used`, `panic` and
   `arithmetic_side_effects` to make that structural.
2. **Never OOM.** Peak allocation is bounded by the `ReaderLimits` the
   target passes, which are deliberately tighter than the defaults.
3. **Bounded allocation.** Allocation follows bytes *read*, never a
   declared length. Also asserted directly, per length-bearing field, in
   `crates/xarast-xar/tests/bounded_allocation.rs` — the fuzzer confirms
   it, the unit test localises it.
4. **Terminate.** Every record consumes at least its eight header bytes,
   so the record loop is structurally finite; `fuzz_xar_records` asserts
   it anyway.
5. **Valid or nothing.** Anything the decoders produce is well formed:
   every decoded path passes `xarast_geom::Path::validate`, every bitmap
   range lies inside its payload, every resolved colour channel is finite.
   At the level of a whole file, `fuzz_xar_import` asserts the stronger
   form the phase document asks for: either `import` fails, or the
   `Document` it returns has **zero** `validate()` errors. It also checks
   that the record accounting balances — mapped plus skipped plus
   stripped is every record read — which is how a handler that visits a
   node twice, or not at all, shows up as a finding rather than as a
   quietly wrong document.

`fuzz_xar_decode` remains as the narrower target: it exercises the same
handlers with no document model behind them, so a crash in it points at
the decoders rather than at the builder.

## The seed corpus is synthetic, and must stay that way

The 59 real `.xar` files are Xara's artwork and may not be copied into
this repository (`docs/11-licensing-and-clean-room.md §3.2`). Every file
under `corpus/` is generated by
`crates/xarast-xar/tests/fuzz_seeds.rs`, and a test in that file fails if
anything else appears there. Regenerate with:

```sh
XARAST_WRITE_FUZZ_SEEDS=1 cargo test -p xarast-xar --test fuzz_seeds
```

A developer who has the corpus locally can seed from it through
`XARAST_XAR_CORPUS` without committing anything.

## When a crash is found

Minimise it with `cargo fuzz tmin`, rebuild it by hand from the minimised
bytes — with `xarast_xar::synth::XarBuilder` for a `.xar` input — and add
it as a regression test in the owning crate, so that it is checked on
every push rather than only nightly. `fuzz/artifacts/` is not committed:
a raw artefact derived from a real-corpus seed could carry its bytes.
`crates/xarast-xar/tests/fuzz_regressions.rs` holds the importer's.

The first runs (2026-09-23) found nine bugs; each is a test now:

| Target | Finding | Test |
|---|---|---|
| `fuzz_xar_import` | an unresolved `TAG_NODE_BITMAP` counted as mapped twice | `an_unresolved_node_bitmap_is_counted_once` |
| `fuzz_xar_import` | children of grid records and of default attributes never counted | `a_subtree_under_a_grid_record_is_accounted_for`, `..._default_attribute_...` |
| `fuzz_xar_tree`, `fuzz_xar_import` | `N DOWN a UP DOWN b UP` dropped `a` from the tree | `a_node_descended_into_twice_keeps_both_groups_of_children` |
| `fuzz_doc_builder` | a dangling colour made `finish` return `Inconsistent` | `a_dangling_colour_reference_is_kept_and_is_only_a_warning` |
| `fuzz_doc_builder` | a sourceless controller at the depth limit did too | `a_sourceless_controller_at_the_depth_limit_is_dropped` |
| `fuzz_ramp` | NaN stop offsets broke the sort's total order | `nan_offsets_do_not_break_the_sort` |
| `fuzz_svg_path_parse` | an 8e77 arc radius asked `kurbo` for 1.8 GB of cubics | `svg_reader_refuses_numbers_beyond_the_extent_before_parsing` |
| `fuzz_hit_test` | a singular 1000x transform over an extent-sized ellipse flattened every cubic crossing the probe band whole (37 s, out of memory) | `hit::tests::huge_curves_through_the_band_are_cheap` |

## Known limits of the targets

- `fuzz_display_list` keeps geometry within ±10 000 000 mp and drops a
  dash pattern that would cut its path into more than 2 000 pieces. The
  CPU backend strokes and dashes a path in document space before clipping
  it to the viewport, so a long, thick, finely dashed stroke at high zoom
  exhausts memory; that is a render gap (`docs/memory/render.md` TODO 11,
  gintrack XARA-T-0022), not a fuzzing artefact, and the bounds stop it
  from masking everything else. Lift both when it is fixed.
- `fuzz_hit_test` keeps the transform's linear part within ±1000 and
  does not assert that a larger pick radius keeps every hit: on a
  zero-area sliver two flattening tolerances legitimately disagree (its
  first finding, with a determinant-0 matrix).
- `fuzz_path_boolean` runs one `BoolOp` × `FillRule` pair per input, not
  all sixteen, because an extent-sized curve flattens to thousands of
  vertices and sixteen overlays of it per case exhaust the time budget.
