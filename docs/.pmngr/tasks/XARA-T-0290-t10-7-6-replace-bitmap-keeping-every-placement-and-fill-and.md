---
id: XARA-T-0290
type: task
title: "T10.7.6: Replace bitmap (keeping every placement and fill) and Save a copy in the bitmap gallery"
status: done
priority: medium
parent: XARA-US-0055
author: mcp
labels: [phase-10, ui]
created: 2026-09-24T11:52:16Z
updated: 2026-10-16T10:29:41Z
started: 2026-10-16T09:52:08Z
closed: 2026-10-16T10:29:41Z
---

## Description
The bitmap gallery (XARA-US-0055) offers Place and Delete. Phase 10 also lists Replace (T10.7.6: a new image takes the resource's place, every bitmap object and every bitmap fill or transparency referring to it follows) and Save a copy (write the stored original bytes through the portal's save chooser).

## Acceptance Criteria
- A `ReplaceBitmap` command in `xarast-doc` swaps the resource behind one `BitmapId` as one undo step (a new history action carrying the old and new resource), exact on undo.
- The gallery's Replace opens the Import chooser for one file and replaces the chosen bitmap; placements keep their parallelograms, fills their geometry.
- Save a copy writes the original bytes (or a PNG of native pixels) under the chosen name.
- Damage: a replace repaints only the objects using the bitmap.

## Notes
Resource insertion is still outside the history (XARA-T-0283); a replace must not be.
