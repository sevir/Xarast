---
id: XARA-T-0319
type: task
title: "perf: re-base render-10k/render-100k gates on whole-frame rasterisation (bench render --whole)"
status: done
priority: high
author: mcp
labels: [perf, ci, render]
created: 2026-09-24T20:11:50Z
updated: 2026-10-15T12:38:54Z
started: 2026-10-15T12:14:12Z
closed: 2026-10-15T12:38:54Z
---

## Description
Found in XARA-T-0314: since the scene diff (XARA-T-0221), plain `xarast-cli bench render` no longer rasterises after the first frame; the render thread finds no damage and reuses every pixel (~0.4–2 ms). The `render-10k` and `render-100k` gates in `xtask/perf-budgets.txt` were calibrated that way, so they no longer guard rasterisation cost. With `--whole`, 10k measures 12–15 ms locally.

## Acceptance Criteria
- `render-10k` / `render-100k` measure whole-frame rasterisation (`--whole`) with re-derived limits (documented margin), or are split into a "reuse" gate and a "rasterise" gate.
- perf.md updated; the old baseline numbers marked as reuse-path measurements.
