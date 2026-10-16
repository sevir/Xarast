---
id: XARA-T-0335
type: task
title: "Blend follow-ups: along a path (F6), live/text/nested ends, step distance, mixed fill types, shadows casting steps"
status: done
priority: medium
parent: XARA-US-0070
author: mcp
labels: [phase-13, live-effects, geom, render]
created: 2026-10-15T22:07:01Z
updated: 2026-10-16T15:00:26Z
started: 2026-10-16T13:20:48Z
closed: 2026-10-16T15:00:26Z
---

## Description
From XARA-T-0333 (d34bc37..6bb30c0):
1. F6 blend along a path (none in corpus), incl. bounds covering the path.
2. Blend ends that are moulds or other live objects (deep moulds fixed in XARA-T-0337), text ends, nested blends; blend ends still use the old `inks`, so a nested contour or blend inside a blend end is left out (contours now use `contour::drawn_inks`, XARA-T-0332).
3. Derive step count from step distance.
4. Mixed fill types between ends the original's way (start's type, colours blended); today switches at halfway.
5. Shadows don't include blend steps in what casts them.
6. §4.6 Case C rasterisation (>64 steps) needs an app-supplied rasteriser.
7. VM comparison of the node-matching rule over 12 pairs (with XARA-T-0325's VM).
From XARA-T-0332: contour rings are generated twice per committed state (bounds cache + walker) — consider sharing for blends too.
