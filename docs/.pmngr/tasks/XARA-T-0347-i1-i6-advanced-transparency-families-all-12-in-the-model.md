---
id: XARA-T-0347
type: task
title: "I1–I6 — Advanced transparency families: all 12 in the model, one LUT source, graduated/bitmap sources, .xar mapping, .xarast"
status: in_progress
priority: medium
parent: XARA-US-0072
author: mcp
labels: [phase-13, render, colour, xar, format]
created: 2026-10-16T01:58:53Z
updated: 2026-10-16T01:58:53Z
started: 2026-10-16T01:58:53Z
---

## Description
Phase-13 §I: I1 `Transparency { family, source: Flat|Gradient|Image }` covering all 12 families; I2 LUTs from one shared source consumed by both backends; I3 grey-conversion weights (needs the VM — document procedure, keep current constants if unverifiable); I4 graduated and bitmap-sourced transparency for every family with profiles and multi-stop ramps; I5 transparency type selector carrying the family in the UI; I6 `.xar` `TranspType` → family mapping (verify enums against the writer) and `.xarast` per research/06 §6.5. Survey what already exists first (render.md blend families, colour.md).

## Acceptance Criteria
- Every family × source renders and round-trips; goldens per family; corpus unchanged or improved with reasons; gates = CI.
