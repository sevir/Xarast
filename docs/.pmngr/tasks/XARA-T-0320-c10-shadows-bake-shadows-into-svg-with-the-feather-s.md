---
id: XARA-T-0320
type: task
title: C10 (shadows) — Bake shadows into SVG with the feather's FilterChain
status: done
priority: medium
parent: XARA-US-0069
author: mcp
labels: [phase-13, live-effects, format, export]
created: 2026-09-24T21:04:11Z
updated: 2026-10-15T12:46:21Z
started: 2026-10-15T12:14:12Z
closed: 2026-10-15T12:46:21Z
---

## Description
XARA-T-0318 records shadows in `.xarast`/SVG only as `<xarast:shadow>` (reported as `Compromise::NotRendered` "shadows (recorded, not baked)"), so resvg, browsers and Inkscape draw objects without their shadows. Reuse `crates/xarast-format/src/svg/effect.rs` (`FilterChain::new(kind, Region)`, `push`, `finish(defs)`) from XARA-T-0317: `feGaussianBlur` on SourceAlpha with σ = penumbra/4 (σ = r/2, r = penumbra/2), `feOffset` by the wall offset (SVG frame, y down), `feMorphology dilate` by the glow width, `feFlood` colour × darkness, composited under SourceGraphic; a floor shadow as a transformed copy. Register the kind in the reader's derived-filter list (`is_known_def` in `read/build/root.rs`, `Reader::is_derived_filter` in `read/build.rs`).

## Acceptance Criteria
- Corpus round trip stays 59/59 with a byte-identical re-save.
- export-check: drop the Groucho2 svg 11.6 and SoftShadow svg 5.8 limits in `xtask/export-limits-corpus.txt`.

## Notes
Needs XARA-T-0317 merged into the branch it is built on.
