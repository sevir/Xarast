---
id: XARA-T-0330
type: task
title: "E1–E5, E7, E8 — Contours as a live effect: params, robust offsetting, steps and colours, import 4066/4067, .xarast round trip"
status: done
priority: high
parent: XARA-US-0070
author: mcp
labels: [phase-13, live-effects, geom, xar, format]
created: 2026-10-15T19:00:19Z
updated: 2026-10-15T20:53:05Z
started: 2026-10-15T19:00:19Z
closed: 2026-10-15T20:53:05Z
---

## Description
Phase-13 §E rows E1 (`ContourParams`), E2 (robust single-step offsetting with joins, self-intersection removal, degenerate input), E3 (steps with bias/gain profile, summed bounds path), E4 (fade/rainbow/alt-rainbow colour interpolation), E5 (inner contours incl. collapse), E7 (`.xar` 4066/4067, currently stripped; 4 controllers in corpus incl. testimp1), E8 (`.xarast` per research/06 §6.8.3, baked as real offset paths). E6 (tool) out of scope.

## Acceptance Criteria
- Contours import and draw; parser never panics on corrupt contour records (fuzz seed).
- Goldens; property tests for offsetting (no panics, bounded output on degenerate input).
- `.xarast` round trip 59/59 byte-identical; export-check no new failures; testimp1 score measured.
