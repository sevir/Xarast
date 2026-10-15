---
id: XARA-T-0341
type: task
title: "Procedural fill follow-ups: perspective field, fill-tool handles and Randomise UI (H7), export size of heavy procedural files, bit-identity check"
status: backlog
priority: low
parent: XARA-US-0071
author: mcp
labels: [phase-13, fills, image, export]
created: 2026-10-15T23:21:54Z
updated: 2026-10-15T23:21:54Z
---

## Description
From XARA-T-0336 (93372a4..897a21d):
- Add a `persp` field to procedural fills so a perspective mould deforms the tile fully (today affine part only).
- H7: fill-tool handles and Randomise UI.
- Export sizes grew: Spitfire SVG 1.3→5.4 MB, Watch4 SVG 1.2→4.3 MB, Watch4 PDF 24→39 MB, SimpleSphere PDF 0.8→3.4 MB; dedup is per distinct tile only.
- Plasma uses MSVC CRT `rand()` LCG (original is platform-dependent): unverified; optionally verify clouds bit-identity with the original.
- `ProceduralSource::cache_key` in xarast-doc unused; bitmap contone ignores bias/gain profile.
