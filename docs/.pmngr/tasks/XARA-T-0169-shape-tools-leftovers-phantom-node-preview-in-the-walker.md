---
id: XARA-T-0169
type: task
title: "Shape tools leftovers: phantom-node preview in the walker, Ctrl+Shift radius mode, T5.5 convert to shapes, VM observation of radius under scale (K8)"
status: in_progress
parent: XARA-US-0033
author: mcp
labels: [phase-7, tools]
created: 2026-09-23T18:11:06Z
updated: 2026-10-16T08:40:54Z
started: 2026-10-16T08:40:54Z
---

## Description
- Creation preview is an overlay outline; a filled live preview needs `Preview::phantom` support in `walker.rs` (owned by another agent this round).
- Constrain+Adjust while drawing is "square from centre" here; the original switches to a radius mode that rotates the shape (`research/04 §4.10`).
- T5.5 Convert to shapes (Ctrl+Shift+S).
- Observe in the VM: corner radius under a non-uniform scale (today the ratio to the major axis is kept), the original's Angle field content, default of "scale lines".
- Generate paths for quick shapes with no cached path (`shapes_pending`, walker side).
