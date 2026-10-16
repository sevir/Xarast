---
id: XARA-T-0355
type: task
title: "Proptest failure: xarast-geom paths.rs hull_contains_tight_bounds (degenerate vertical cubic)"
status: done
priority: medium
author: mcp
labels: [geometry, bug]
created: 2026-10-16T11:43:23Z
updated: 2026-10-16T12:46:37Z
started: 2026-10-16T12:06:51Z
closed: 2026-10-16T12:46:37Z
---

## Description
Random case seen during XARA-T-0203 gates (unrelated to that change; passed on re-run): `crates/xarast-geom/tests/paths.rs:582` `assert!(p.bounds().contains_rect(p.tight_bounds()))` fails for MoveTo(0,0) CubicTo((0,-64471),(0,88035),(0,88035)) — a degenerate vertical cubic whose extremum lies between the control points. Likely rounding of the tight extremum (millipoint quantisation) stepping outside the control-point hull, or the root solve. Find the root cause (not tolerance loosening); add the case to `paths.proptest-regressions`; run the property at ≥ 20 000 cases.
