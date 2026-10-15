---
id: XARA-T-0326
type: task
title: "Bevel follow-ups: join attribute for outer bevels, first-frame cost (cache the lighting map), multi-fill inner bevels, resolution-independent SVG bake"
status: backlog
priority: medium
parent: XARA-US-0069
author: mcp
labels: [phase-13, live-effects, render, perf]
created: 2026-10-15T13:50:43Z
updated: 2026-10-15T13:50:43Z
---

## Description
Leftovers from XARA-T-0322 (render.md TODOs 31–32):
- Outer-bevel joins are always round; read the join attribute for mitre/bevel.
- First frame cost: Groucho2 first frame 73 → 109 ms, peak memory 62 → 77 MiB, one 400 % frame ≈ 1 s. Cache the lighting map separately from content colour.
- Inner bevel lights each object's own colours; the original paints the first object's fill over the band (differs for multi-fill groups or a stroke under the band).
- SVG bevel bake depends on resolution; extend research/06 §6.8.2 with the 15-style vocabulary.
