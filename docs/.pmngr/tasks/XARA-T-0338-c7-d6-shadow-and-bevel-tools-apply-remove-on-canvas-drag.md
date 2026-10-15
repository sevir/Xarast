---
id: XARA-T-0338
type: task
title: "C7, D6 — Shadow and Bevel tools: apply/remove, on-canvas drag handles, infobar with numeric fields, undo"
status: done
priority: high
parent: XARA-US-0069
author: mcp
labels: [phase-13, live-effects, tools, ui, app]
created: 2026-10-15T22:43:34Z
updated: 2026-10-15T23:20:43Z
started: 2026-10-15T22:43:34Z
closed: 2026-10-15T23:20:43Z
---

## Description
Phase-13 rows C7 (shadow tool: wall/floor/glow, offset/angle/size drag, blur, darkness, colour) and D6 (bevel tool: indent, light angle by dragging the light on canvas, tilt, contrast, type, join, inner/outer), with phase-12 accessibility rule J2 (every parameter reachable numerically in the infobar). Parameter edits are commands with one undo step per gesture; regeneration is not an undo step (A11). Apply/remove shadow and bevel to the selection as commands.

## Acceptance Criteria
- Tools registered with infobars and cursors; drag gestures and numeric fields edit `ShadowParams`/`BevelParams` through the command bus; undo restores exactly.
- Headless tool-machine tests (no real windows); goldens of a shadowed/bevelled object after a scripted gesture.
- Gates = CI.
