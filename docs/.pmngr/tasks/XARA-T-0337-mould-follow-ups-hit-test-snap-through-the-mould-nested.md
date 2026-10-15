---
id: XARA-T-0337
type: task
title: "Mould follow-ups: hit-test/snap through the mould, nested effects inside moulds, deep mould blend ends, font-aware cache key, affine shortcut, convert to shapes"
status: backlog
priority: medium
parent: XARA-US-0071
author: mcp
labels: [phase-13, live-effects, geom, app]
created: 2026-10-15T22:43:28Z
updated: 2026-10-15T22:43:28Z
---

## Description
From XARA-T-0334 (9d38c29..7729e6b):
- Hit-testing and snapping inside a mould see the undeformed source; map the pointer through the bounded inverse.
- A blend end containing a mould below its top node blends the undeformed source.
- Shadows, bevels and feathers inside a mould are not moulded.
- Walker mould cache key ignores fonts (font change doesn't re-mould text).
- SVG single-`transform` shortcut for affine perspectives (§6.8.5) not done.
- "Convert to shapes" (BecomeA) for moulds not wired.
- 2×2 envelope is Xarast's own definition (original smooths inner lines; never from `.xar`).
- Export scores rose with new ink: Watch4 svg 0.72→0.91, pdf 1.03→1.24; TextCurve svg 3.06→3.20, pdf 3.46→3.84.
