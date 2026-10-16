---
id: XARA-T-0348
type: task
title: "Phase 13 conformance: golden coverage per parameter, effect perf budgets, offset proptest at 10k, i18n and keyboard-only checks"
status: in_progress
priority: medium
parent: XARA-US-0072
author: mcp
labels: [phase-13, render, perf, tests]
created: 2026-10-16T02:49:08Z
updated: 2026-10-16T02:49:08Z
started: 2026-10-16T02:49:08Z
---

## Description
Audit phase-13 acceptance criteria (docs/phases/phase-13-live-effects.md "Acceptance criteria" 1–17 and "Performance budgets") against what exists, and close the cheap gaps: ≥60 effect goldens incl. every parameter at min/typical/max (AC2), nesting goldens (AC4), become_shapes coverage (AC9), offsetting property tests at 10k cases (AC10), fractal determinism on both arches (AC11, at least documented), effect perf budgets in xtask bench/perf (AC13–14), keyboard-only parameter access tests for the five tools (AC15), i18n check (AC16). Record the AC status table in docs/memory (which are met, which need the VM/Chromium/Inkscape/lavapipe).

## Acceptance Criteria
- AC status table committed; gaps closed or filed; gates = CI.
