---
id: XARA-T-0337
type: task
title: "Mould follow-ups and J4 nesting: effects inside moulds, deep mould blend ends, edit inside a mould, font-aware key, affine shortcut, convert to shapes, nesting composition order"
status: done
priority: medium
parent: XARA-US-0071
author: mcp
labels: [phase-13, live-effects, geom, app]
created: 2026-10-15T22:43:28Z
updated: 2026-10-16T01:58:37Z
started: 2026-10-16T00:50:15Z
closed: 2026-10-16T01:58:37Z
---

## Description
From XARA-T-0334 (9d38c29..7729e6b) and XARA-T-0342 (03cbf4d..ce522d7):
- Shadows, bevels and feathers inside a mould are not moulded.
- A blend end containing a mould below its top node blends the undeformed source.
- Editing objects inside a mould (shape editor through `MouldTransform::source_point`); snapping to moulded outlines; source rectangle does not follow edits made inside the mould. (Picking by moulded outline is DONE in 8b643f1; text picked only by box.)
- Walker mould cache key ignores fonts.
- SVG single-`transform` shortcut for affine perspectives (§6.8.5).
- "Convert to shapes" (BecomeA, A9) for live objects — at least moulds.
- Mould tool leftovers: paste any clipboard path as a mould; Remove+Constrain copies the cage; 2×2 envelope preset.
- Phase-13 J4: a shadow on a blend on a moulded group must work — define and test the composition order.
- Export scores rose with new ink: Watch4 svg 0.72→0.91, pdf 1.03→1.24; TextCurve svg 3.06→3.20, pdf 3.46→3.84.
