---
id: XARA-T-0328
type: task
title: "Bevel leftovers: resolution-independent SVG bake following joins and inner paint; inner-band concave corners and transparency copy"
status: done
priority: low
parent: XARA-US-0069
author: mcp
labels: [phase-13, live-effects, render, export]
created: 2026-10-15T17:09:40Z
updated: 2026-10-16T02:49:08Z
started: 2026-10-16T01:19:57Z
closed: 2026-10-16T02:49:08Z
---

## Description
From XARA-T-0326 (6f3060e..dd78ca7; render.md TODOs 35–36, xarast-format.md):
- SVG bevel bake is resolution-dependent and ignores the new joins and inner paint (testimp1 SVG 0.49 → 0.52). A user-space-only chain is written up in xarast-format.md; needs re-tuning at two DPIs. `kernelUnitLength` is ignored by Chromium and resvg.
- Inner bands round their concave corners whatever the join; the bevel node's transparency copy is not applied to a painted inner band.
- Corners are detected from 2–8 px of outline, so very small shapes still round.
- Damage property generator does not push bevels.
