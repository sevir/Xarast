---
id: XARA-T-0321
type: task
title: "Nested effects in a silhouette pass: a feather around a shadow erodes the shadow away"
status: done
priority: low
parent: XARA-US-0069
author: mcp
labels: [phase-13, live-effects, render]
created: 2026-09-24T21:04:11Z
updated: 2026-10-15T16:10:02Z
started: 2026-10-15T15:37:55Z
closed: 2026-10-15T16:10:02Z
---

## Description
The CPU silhouette pass composites a nested effect's colour result with its alpha, so a feather wrapping a shadow controller sees the half-transparent shadow in its silhouette and its erosion removes most of it (`effect_shadow_feathered` golden). Decide what the original's silhouette of a shadow is (the shadow node is a bitmap-transparency path) and make the silhouette pass follow it. Also covers inner shadows (C5): the model has no `Inner` kind and `.xar` has no inner shadow.

## Acceptance Criteria
- A documented rule for nested effects in silhouette passes, with a golden.

## Notes
No corpus file puts a feather on or around a shadow controller.
