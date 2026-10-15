---
id: XARA-T-0334
type: task
title: "G1–G8, G10, G11 — Moulds as a live effect: envelope and perspective maths, fills/bitmaps/text under a mould, import 107–110/4012, .xarast"
status: in_progress
priority: high
parent: XARA-US-0071
author: mcp
labels: [phase-13, live-effects, geom, xar, format]
created: 2026-10-15T21:24:56Z
updated: 2026-10-15T21:24:56Z
started: 2026-10-15T21:24:56Z
---

## Description
Phase-13 §G rows G1 (`MouldParams` Envelope/Envelope2x2/Perspective), G2 (`MouldGeometry`), G3 (envelope Bézier patch with adaptive subdivision), G4 (perspective with affine-degeneracy detection), G5 (`MouldTransform`, non-invertible), G6 (fills deform with the object), G7 (bitmaps under a mould), G8 (text under a mould: curves baked into generated subtree), G10 (`.xar` 107, 108, 109, 110, 4012), G11 (`.xarast` per research/06 §6.8.5 with `<xarast:mould-source>`). G9 (tool) out of scope. Watch4 has 126 moulds.

## Acceptance Criteria
- Moulds import and draw; parser never panics on corrupt mould records (fuzz seed).
- Goldens; property tests (corners map exactly, no panics on degenerate quads).
- `.xarast` round trip 59/59; export-check no new failures; affected corpus files measured (Watch4).
