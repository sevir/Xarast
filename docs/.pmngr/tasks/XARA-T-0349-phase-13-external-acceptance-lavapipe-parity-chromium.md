---
id: XARA-T-0349
type: task
title: "Phase 13 external acceptance: lavapipe parity, Chromium/Inkscape render check, VM review, aarch64 determinism, i18n tool, manual"
status: backlog
priority: low
parent: XARA-US-0072
author: mcp
labels: [phase-13, ci, external]
created: 2026-10-16T03:21:34Z
updated: 2026-10-16T03:21:34Z
---

## Description
From the XARA-T-0348 audit (render.md "Phase 13 conformance audit"): AC3 (GPU↔CPU parity on a lavapipe runner), AC6 (`check-external-render` with Chromium + Inkscape, ΔE₀₀ < 4), AC7 (VM review against the original), AC9 measured in ΔE₀₀ rather than channel mean, AC11 aarch64 determinism runner, AC13 frame/Draft budgets with committed scenes + GPU blur + 512² fractal, AC16 `xtask i18n`, AC17 mdbook manual chapters and keyboard reference.
