---
id: XARA-T-0343
type: task
title: "Contour/blend tool follow-ups: point-mapping and remap drags, steps/distance toggle, inner width clamp, new-contour fills for open paths, handle snapping, heavy-blend confirmation"
status: done
priority: low
parent: XARA-US-0070
author: mcp
labels: [phase-13, tools, app]
created: 2026-10-16T00:08:37Z
updated: 2026-10-16T16:40:47Z
started: 2026-10-16T15:00:32Z
closed: 2026-10-16T16:40:47Z
---

## Description
From XARA-T-0340 (f229b70..cef39e6; tools.md Open TODOs): point-mapping and remap drags; contour steps/distance toggle; clamp inner contour width to the deepest possible inset; original's fill for open paths and cloned fill when the object has none on new contours; snapping of size handles; refuse removing a bevelled/contoured blend; original's confirmation for heavy blends; common attributes not factored onto a new blend controller (objects localised instead).
From XARA-T-0335 (dc0902f..3beae58): blend tool attach/detach path (original also moves the ends onto the path when attaching); more than one path per blend; object profile direction on a reversed pair; the original's mixed-fill rule for contours (contour attribute blending uses the same calls as blends).
