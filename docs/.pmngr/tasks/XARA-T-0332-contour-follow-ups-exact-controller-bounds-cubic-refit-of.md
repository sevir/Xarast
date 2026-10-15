---
id: XARA-T-0332
type: task
title: "Contour follow-ups: exact controller bounds, cubic refit of rings, text sources, nested live output, inset source in SVG"
status: backlog
priority: medium
parent: XARA-US-0070
author: mcp
labels: [phase-13, live-effects, geom, format]
created: 2026-10-15T20:53:05Z
updated: 2026-10-15T20:53:05Z
---

## Description
From XARA-T-0330 (8a29a92..d2761cc):
- Outer contour controller bounds are an overestimate (distance × 11.47 mitre); culling, damage and fit-drawing are oversized. Use the regenerated rings.
- Rings are polylines; refit to cubics.
- Text sources are not contoured; a nested live object's generated output is left out of the silhouette.
- In SVG, an inset contour's source is still drawn by external viewers (the walker hides it).
- Export scores rose slightly with new ink: testimp1 svg 0.52→0.58, pdf 0.89→1.05; Girard_simple svg 0.28→0.32, pdf 1.05→1.09.
