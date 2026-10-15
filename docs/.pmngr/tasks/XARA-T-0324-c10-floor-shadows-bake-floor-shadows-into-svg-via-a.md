---
id: XARA-T-0324
type: task
title: C10 (floor shadows) — Bake floor shadows into SVG via a transformed copy the reader ignores
status: backlog
priority: low
parent: XARA-US-0069
author: mcp
labels: [phase-13, live-effects, format, export]
created: 2026-10-15T12:46:21Z
updated: 2026-10-15T12:46:21Z
---

## Description
6c5e8e1 (XARA-T-0320) bakes wall and glow shadows through `effect::shadow`. Floor shadows are still reported `NotRendered` ("shadows (recorded, not baked)"): an SVG filter cannot apply the renderer's affine map. Needs a transformed `<use>` copy in the generated group, filtered, plus a reader rule ignoring that copy. Also: a controller with both a shadow and a feather cannot carry two `filter`s (none in corpus); shadow on text uses a ±50% filter region.

## Acceptance Criteria
- Floor shadows visible in resvg; `.xarast` round trip stays byte-identical; count floors across the corpus first.
