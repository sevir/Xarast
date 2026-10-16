---
id: XARA-T-0263
type: task
title: T9.3.8 — Centre, right and decimal tab stops in layout
status: done
parent: XARA-US-0046
author: mcp
labels: [phase-9, text]
created: 2026-09-24T01:52:49Z
updated: 2026-10-16T15:21:27Z
started: 2026-10-16T14:08:19Z
closed: 2026-10-16T15:21:27Z
---

## Description
Layout treats every tab stop as a left stop. Since XARA-T-0225 the text ruler can set centre, right and decimal stops (`kind & 3` = 2, 1, 3), which are stored and round-trip but lay out as left stops. The model also lacks the decimal stop's decimal-point character (`text.md`, "Ruler record").

## Acceptance Criteria
- Right, centre and decimal stops align the tab section that follows them as the original does (`Kernel/nodetxtl.cpp` formatting; facts only, clean room).
- The decimal character is in `TabStop` and in the `.xarast` ruler text; `.xar` import reads it.
- Tests in `xarast-text` for each kind, and one corpus/ruler round trip.

## Notes
Found while doing XARA-T-0225 (the ruler UI offers the kinds already).
