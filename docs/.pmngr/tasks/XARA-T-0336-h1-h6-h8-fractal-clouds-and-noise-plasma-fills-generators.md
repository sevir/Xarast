---
id: XARA-T-0336
type: task
title: "H1–H6, H8 — Fractal (clouds) and noise (plasma) fills: generators, deterministic PRNG, cached materialisation, import and .xarast"
status: done
priority: medium
parent: XARA-US-0071
author: mcp
labels: [phase-13, fills, image, xar, format]
created: 2026-10-15T22:07:12Z
updated: 2026-10-15T23:21:54Z
started: 2026-10-15T22:07:12Z
closed: 2026-10-15T23:21:54Z
---

## Description
Phase-13 §H rows H1 (`FractalParams`, `NoiseParams`), H2 (diamond-square generator with the documented per-level rule), H3 (tileable + squash), H4 (Perlin-style plasma), H5 (materialise to an `Image` paint cached by the full parameter tuple; `Randomise` command), H6 (deterministic PRNG, same bitmap everywhere), H8 (`.xar` fill attribute import; `.xarast` parameters plus baked raster). H7 (fill tool) out of scope.

## Acceptance Criteria
- Corpus survey of fractal/noise fills first; they import and draw; parser never panics on corrupt records.
- Determinism test (same seed ⇒ identical bytes across runs/threads); goldens.
- `.xarast` round trip 59/59; export-check no new failures; affected files measured.
