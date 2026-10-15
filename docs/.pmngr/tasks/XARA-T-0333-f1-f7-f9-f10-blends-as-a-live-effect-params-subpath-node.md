---
id: XARA-T-0333
type: task
title: "F1–F7, F9, F10 — Blends as a live effect: params, subpath/node correspondence, interpolation, along a path, import, .xarast"
status: done
priority: high
parent: XARA-US-0070
author: mcp
labels: [phase-13, live-effects, geom, xar, format]
created: 2026-10-15T20:53:13Z
updated: 2026-10-15T22:07:00Z
started: 2026-10-15T20:53:13Z
closed: 2026-10-15T22:07:00Z
---

## Description
Phase-13 §F rows F1 (`BlendParams`, multi-object controller), F2 (subpath correspondence incl. one-to-one), F3 (node correspondence), F4 (geometry interpolation with position profile), F5 (attribute interpolation with attribute profile; fade/rainbow/alt-rainbow), F6 (blend along a path), F7 (steps generated at render time, not tree nodes), F9 (`.xar` 105, 106, 4060–4062, 4072–4074), F10 (`.xarast` per research/06 §6.8.4 incl. blend-map and the >64-step rule). F8 (tool) out of scope. Reuse the contour work's `LiveCache`/`RegenKey` path in the walker (XARA-T-0330) and the colour blending in `xarast_doc::contour`.

## Acceptance Criteria
- Blends import and draw; parser never panics on corrupt blend records (fuzz seed).
- Goldens; property tests for correspondence/interpolation.
- `.xarast` round trip 59/59 byte-identical; export-check no new failures; affected corpus files measured.
