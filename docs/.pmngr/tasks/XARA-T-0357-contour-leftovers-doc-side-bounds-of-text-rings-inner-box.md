---
id: XARA-T-0357
type: task
title: "Contour leftovers: doc-side bounds of text rings, inner box vs wide ring stroke, share ring generation between bounds cache and walker"
status: backlog
priority: low
parent: XARA-US-0070
author: mcp
labels: [phase-13, live-effects, geom]
created: 2026-10-16T13:20:48Z
updated: 2026-10-16T13:20:48Z
---

## Description
From XARA-T-0332 (7c8c0b3..ecd9dc7): the document has no fonts, so it can't bound rings of a contour of text (only the walker covers them); an inner contour's box ignores a ring stroke wider than its inset (no corpus case); rings are generated twice per committed state (bounds cache + walker) without a shared cache; `RegenQueue` still unused.
