# Project memory index

Durable per-subsystem notes. Any agent working on a subsystem **must** read its
note first and update it afterwards.

| Note | Subsystem | Covers |
|---|---|---|
| `geometry.md` | Geometry & colour | Millipoints, paths, booleans, colour models, palettes |
| `colour.md` | Colour editing (`xarast-color`, `xarast-doc` palette/fill commands) | Conversions vs. the original, palette editing and its DAG, epochs, fill/transparency commands, the corpus oracle |
| `xar-import.md` | `.xar` importer | Physical layer, tag coverage, format quirks, corpus acceptance numbers |
| `document-model.md` | Document model | Node arena, attributes, invariants, undo |
| `app-core.md` | Application core | `EditState`, `Viewport`, the arena→`Scene` walker, sessions, command dispatch, the shell/UI contract |
| `tools.md` | Editing & tools | Command bus in the app, tool machine, tools, preview, palette, infobar, undo labels |
| `render.md` | Render engine | Pipeline, blend modes, caching, CPU/GPU decisions |
| `xarast-format.md` | Native format | Versioning, round-trip, SVG extensions |
| `ui.md` | User interface | Toolkit, panel layout, shortcuts, Wayland |
| `packaging.md` | Packaging | AppImage, CI, glibc compatibility |
| `text.md` | Text | Shaping, fonts, text on a path |
| `image.md` | Images (`xarast-image`) | Decoding façade, decode limits and bomb fixtures, EXIF, `.xar` bitmap wrappings, the walker contract |
| `export.md` | Export (`xarast-io`) | Export model and sizing, filter registry, raster encoders, PDF (crate spike, fidelity matrix, the ladder), SVG (the interchange dialect of the profile mapper), determinism contract, `Compromise` taxonomy, the `.xar` non-goal |
| `perf.md` | Performance | Benchmarks, budgets, regressions |

## Local gates = CI

The "Format, lint and test" job of `.github/workflows/ci.yml`, run locally.
Run all four, serially, before calling work done. Each one must exit 0.
The job sets `RUSTFLAGS=-D warnings` for every step, and it needs Poppler
(`pdftoppm`) or the PDF render comparisons skip. Leave `XARAST_GPU_TESTS`
unset: the GPU tests serialise on their own lock.

```sh
export RUSTFLAGS="-D warnings"
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
```

`cargo deny check licenses bans sources advisories` is the `licences`
job; run it too when `Cargo.lock` changes.

The doc build is the one people forget: it failed on every push from
fc6eb54 (2026-09-23) to 8645d27 (2026-09-24) without anyone noticing.
Intra-doc links from a public item to a private one
(`rustdoc::private_intra_doc_links`) are errors under `-D warnings`.
Write a private name as a plain code span (`` `walker::ready_image` ``),
never as a link. Never allow the lint.
Where a module and a function share a name (`save`, `open`, `sniff` in
`xarast-format`), link with `mod@name` or `name()`. Add `--keep-going`
to see every crate's errors in one run.

The other per-push jobs are `export` and `reproducible` (the fixture
export, see `export.md`), `perf` (`xtask perf --tier pr`, see `perf.md`)
and `licences`. `fuzz.yml`, `export-corpus.yml` and `perf-nightly.yml` run on a schedule.
GitHub only schedules workflows that are on the default branch (`main`),
so none of them run until the branch is merged.

## Pause state (2026-10-16)

Development is paused at the maintainer's request. The dev branch
`claude/xara-xtreme-rust-port-ktggmu` is merged into `main` (fast-forward)
and CI is green. No task is left half done: every `in_progress` task in the
tracker was closed or its remainder filed.

**Landed in the 2026-10-15/16 run** (tracker ids, details in each note):

- Phase 13 live effects end to end: bevels (T-0322, T-0326, T-0328),
  shadows in SVG incl. floor (T-0320, T-0324), nested-effect silhouettes
  and composition order (T-0321, T-0337), shadows from rendered alpha
  (T-0327), contours (T-0330, T-0332), blends incl. along a path (T-0333,
  T-0335), moulds (T-0334), clouds/plasma fills (T-0336), transparency
  families (T-0347), blur above the ceiling and Draft effects (T-0315),
  the five effect tools (T-0338, T-0339, T-0340, T-0342, T-0343),
  conformance audit (T-0348; AC table in `render.md`).
- Fidelity fixes: `.xar` cap/join/winding bytes are the original's
  in-memory enums; bevelled default join; even-odd default fill rule
  (T-0329, T-0331); palette colours resolved with FIXED24 packing and a
  first-class "no colour" (T-0203, T-0204).
- App: File › Export dialog off the UI thread (T-0192, T-0350), selector
  and path nudges/flip/copy-and-transform (T-0170, T-0186), shape tool
  leftovers (T-0169), path/guide snapping (T-0189), phase-8 command wiring
  (T-0212), bitmap Replace/Save a copy and undoable insertion (T-0290,
  T-0283), text tab stop kinds (T-0263), text-on-a-path editing (T-0251).
- Durability: `xarast repair` and partial XML recovery (T-0088), the
  operation journal (T-0217), save-path speed-ups (T-0215, still open:
  re-measure on a quiet machine).
- Housekeeping: perf gates re-based on whole frames (T-0319), packaging
  notices and build id (T-0194, T-0309), expected-panic marker crate
  `xarast-unwind` (T-0310), two proptest root causes (T-0191, T-0344,
  T-0355), yanked `yoke-derive` and `zip` (zip moved to 9.0.3).

**Suggested next, in order:** XARA-T-0323 (quiet-machine re-measure:
perf gates, ProbeX16 save, effect gates), XARA-T-0356 (palette-exact
SVG/.xarast colours), XARA-T-0353 (journal follow-ups incl. the
first-round-trip digest change of 58/59 corpus files), the leftover tasks
filed under US-0069..0072 (XARA-T-0346, T-0349, T-0357, T-0358, T-0341),
then the backlog of phases 7–11 (`list_items` with category todo/backlog).
Items needing a VM running the original are T-0325, US-0018 and the
"VM" notes in `tools.md`/`render.md`.

**How to resume (lead):** read this index and the subsystem notes; run
agents in worktrees, at most two at a time, each with its own `target/`
and its own scratch subdirectory; ff-merge after
`git merge-base --is-ancestor`, delete the worktree right after; run the
"Local gates = CI" above; check `gh run list` after every push.

## Status

Notes are created as their phase starts. If one does not exist yet, create it
from this template:

```markdown
# <subsystem>

## Current state
## Decisions taken (and why)
## Invariants that must not be broken
## Dead ends (do not retry)
## Open TODOs
```
