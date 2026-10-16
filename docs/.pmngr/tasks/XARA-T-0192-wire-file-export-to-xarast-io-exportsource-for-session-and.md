---
id: XARA-T-0192
type: task
title: "Wire File › Export to xarast-io: ExportSource for Session and the export dialog"
status: done
priority: medium
parent: XARA-US-0056
author: mcp
labels: [phase-11, app, ui, io]
created: 2026-09-23T19:49:04Z
updated: 2026-10-16T05:30:05Z
started: 2026-10-16T03:21:34Z
closed: 2026-10-16T05:30:05Z
---

## Description
`xarast-io` now has the export model (`ExportRequest`, `ExportArea`, `ExportSizing`, `Background`), per-format options, the `Exporter` trait with `Capabilities`, a `Registry`, and PNG/JPEG/WebP exporters (XARA-US-0056/0057). `xarast-io` sits below `xarast-app` in the crate graph, so it takes the document through the `ExportSource` trait (`crates/xarast-io/src/source.rs`): resolve an `ExportArea` to a document rectangle, give the paper colour, build a whole-document `Scene` + `Resolver` with no culling and no overlays.

Today the only implementation over a real document is `SessionSource` in `crates/xarast-cli/src/export.rs` (used by `xarast-cli export`). It belongs in the application core.

## Work
1. `xarast-app`: `impl xarast_io::ExportSource for Session` (move `SessionSource` from the CLI; keep the CLI calling it). Selection area = `EditState::selection_bounds`; page = first page of the active spread or the given page node; spread = `spread_rect`; drawing = `drawing_or_page_rect`. Report walker shortfalls (`WalkStats`) and font substitutions as `Compromise`s, as the CLI adapter does. Use the session's shared `FontService`.
2. Run exports off the UI thread; pass a `Progress` (e.g. `CancelFlag`) so the dialog can show progress and cancel. Cancellation latency is one band.
3. `xarast-ui`: the File › Export dialog (phase 11 T11.1.8), generated from `Exporter::capabilities()` (alpha → background choice, `has_dpi` → dpi field, `lossy` → quality slider, `max_side` → validation), with the size/DPI/physical linkage driven by `ExportSizing::edit` (≈ 24 ns per keystroke, well under the 100 µs budget). JPEG must say that transparency is flattened. Show `ExportReport::compromises` after the export.
4. Delete `SessionSource` from the CLI once the app impl exists.

## Acceptance Criteria
- File › Export writes PNG/JPEG/WebP byte-identical to `xarast-cli export` with the same options.
- The dialog shows no field the format's `Capabilities` does not declare.
- Cancel leaves no file behind.

## Notes
See `docs/memory/export.md`.
