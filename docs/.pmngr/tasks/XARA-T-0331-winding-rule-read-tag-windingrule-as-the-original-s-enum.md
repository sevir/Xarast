---
id: XARA-T-0331
type: task
title: "Winding rule: read TAG_WINDINGRULE as the original's enum and default to even-odd"
status: done
priority: high
author: mcp
labels: [document-model, xar, render, fidelity]
created: 2026-10-15T20:25:53Z
updated: 2026-10-15T21:24:50Z
started: 2026-10-15T20:25:53Z
closed: 2026-10-15T21:24:50Z
---

## Description
Found in XARA-T-0329 (same bug class as caps/joins, fixed in ea7b5ce): `TAG_WINDINGRULE` stores the original's raw value — 0 nonzero, 1 negative, 2 even-odd, 3 positive (`Kernel/attr.h:169-175`, `lineattr.cpp:3506`, `:4276`) — but `FillRule::from_byte` uses a 1..4 table. The original's default is even-odd (`attr.cpp:1858`) and its renderer fills even-odd unless told otherwise (`wxOil/grndrgn.cpp:2233-2234`); Xarast defaults to non-zero. The corpus's 3 winding records are all value 1 (negative), read as non-zero.

## Acceptance Criteria
- Importer maps the 0..3 enum; negative/positive winding handled (or documented approximation).
- Default fill rule even-odd for `.xar` import and new documents; `.xarast` keeps meaning of old files (missing fill-rule attribute semantics decided and documented).
- Goldens/export limits/preview scores checked; every move recorded with reason. Decision in document-model.md.
