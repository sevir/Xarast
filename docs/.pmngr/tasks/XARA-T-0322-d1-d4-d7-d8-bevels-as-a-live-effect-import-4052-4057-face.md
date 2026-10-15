---
id: XARA-T-0322
type: task
title: "D1–D4, D7, D8 — Bevels as a live effect: import 4052–4057, face generation, lighting, .xarast round trip"
status: done
priority: high
parent: XARA-US-0069
author: mcp
labels: [phase-13, live-effects, render, xar, format]
created: 2026-10-15T12:14:08Z
updated: 2026-10-15T13:50:43Z
started: 2026-10-15T12:14:08Z
closed: 2026-10-15T13:50:43Z
---

## Description
Bevels are the biggest remaining visual gap: 22 controllers in the corpus (Groucho2 has 18 inside its shadows), still stripped on import. Implement phase-13 §D rows D1 (`BevelParams`), D2 (face generation per profile), D3 (lighting map), D4 (Bevel transparency family LUT), D7 (`.xar` tags 4052–4057 → params) and D8 (`.xarast` round trip per research/06 §6.8.2). D5 (bevel fill / inking node) if needed for corpus fidelity; D6 (tool) is out of scope.

## Acceptance Criteria
- Bevel controllers import (no longer stripped); parser never panics on corrupt bevel records (fuzz target covers them).
- Bevels render through the offscreen effect pipeline/cache; goldens added.
- `.xarast` round trip stays 59/59 byte-identical; export-check has no new failures.
- Groucho2 preview score improves vs 38.9; measured and recorded.
- Local gates = CI pass; `docs/memory/{render,xar-import,xarast-format}.md` updated.
