---
id: XARA-T-0344
type: task
title: "Proptest failure again: hit.rs stroke_matches_the_renderers_outline under a scaling matrix (second seed)"
status: done
priority: medium
author: mcp
labels: [geometry, bug]
created: 2026-10-16T00:33:01Z
updated: 2026-10-16T01:19:57Z
started: 2026-10-16T00:33:01Z
closed: 2026-10-16T01:19:57Z
---

## Description
XARA-T-0191 (4b2e827) fixed one seed by making the stroker accuracy `0.25 / smax`. A new random case fails (found during XARA-T-0327): path MoveTo(0,0) LineTo(-5991,3190) CubicTo((4469,9806),(-7932,6379),(655,-6977)); matrix a=1.8076640687970629 d=1.7868472749611346 (non-uniform scale); width 1552; caps (0,0); join 0 mitre; mitre limit 1.0; not dashed; point (-8482,10824); r=1572.579287380517. Seed `cc 602a5bdc784787e86428d20c392ac915f0862f5008312703d2c5ab27b9ad9d3a`.

## Acceptance Criteria
- Root cause found (not tolerance loosening); seed added to hit.proptest-regressions and passing; extended run (PROPTEST_CASES ≥ 20000) clean.
