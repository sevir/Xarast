---
id: XARA-T-0315
type: task
title: B7 + J3 — Blur above the 100 px ceiling at reduced resolution; Draft-quality effects during drags
status: done
priority: low
parent: XARA-US-0068
author: mcp
labels: [phase-13, live-effects, render]
created: 2026-09-24T18:06:47Z
updated: 2026-10-16T03:58:02Z
started: 2026-10-16T02:49:20Z
closed: 2026-10-16T03:58:02Z
---

## Description
`xarast_render::blur` clamps every radius to `MAX_RADIUS_PX` (100 px, the original's `MAX_SHADOW_BLUR`). The original does not simply clamp a feather: above a 200 px diameter it renders the silhouette at a reduced resolution and scales the mask back up, so a large feather at high zoom keeps its apparent width (`Kernel/fthrattr.cpp`, `CreateSilhouetteBitmap`, "FeatherScaleFactor"; facts only). Ours visibly narrows past the ceiling.

## Acceptance Criteria
- Above the ceiling the effect region is rendered at 1/k resolution (k = requested radius / 100), blurred at ≤ 100 px, and resampled to device space.
- The `effect_feather_ceiling` golden is re-blessed with a justification; a zoom sweep shows the fade width proportional to zoom across the ceiling.
- Exactness under repaint still holds (render.md invariant 23).

## Notes
Clamp policy and the current limitation: `docs/memory/render.md`, TODO 26.
