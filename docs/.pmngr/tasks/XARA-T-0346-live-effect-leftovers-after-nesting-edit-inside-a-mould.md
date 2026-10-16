---
id: XARA-T-0346
type: task
title: "Live-effect leftovers after nesting: edit inside a mould, effects to shapes, nested contour silhouettes, SVG mould gaps"
status: backlog
priority: low
parent: XARA-US-0071
author: mcp
labels: [phase-13, live-effects, app, format]
created: 2026-10-16T01:58:44Z
updated: 2026-10-16T01:58:44Z
---

## Description
From XARA-T-0337 (64f81e6..84481e5; document-model decisions 54–55):
- Editing objects inside a mould (shape editor through `MouldTransform::source_point`, handles, snapping; source rectangle following inner edits).
- Convert shadows and bevels to shapes (original makes a bitmap-filled path: needs renderer pixels as a bitmap resource).
- Contours don't include nested contour rings or blend steps in their silhouette (original does).
- Blend ends that are shadows/bevels/contours blend their objects, not what they draw.
- SVG single-`transform` shortcut for affine perspectives (id duplication / `<use>` into foreign element concerns).
- SVG export doesn't draw feathers on moulded shapes.
- Mould tool: paste any clipboard path as a mould; Remove+Constrain copies the cage; 2×2 envelope preset.
