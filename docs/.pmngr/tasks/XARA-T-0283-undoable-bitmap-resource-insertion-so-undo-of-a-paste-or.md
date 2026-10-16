---
id: XARA-T-0283
type: task
title: Undoable bitmap resource insertion, so undo of a paste or placement is digest-exact
status: done
priority: low
parent: XARA-US-0052
author: mcp
labels: [phase-10, bitmap, document-model]
created: 2026-09-24T10:08:47Z
updated: 2026-10-16T14:08:19Z
started: 2026-10-16T12:46:37Z
closed: 2026-10-16T14:08:19Z
---

## Description
Gap found closing XARA-T-0272. Placing a bitmap (`Session::place_image`) and pasting a fragment with bitmaps (`structure::import_bitmaps`) add the bitmap resource **outside** the undo history (`tools.md` decisions 41, 68). `Document::canonical_digest` hashes the resource table, so after undo the digest differs from before by that unreferenced resource until a sweep removes it. The tree, attributes and everything else are restored exactly (`tests/place_bitmap.rs` pre-registers the resource to show it).

## Acceptance Criteria
- Inserting a bitmap resource can be part of a transaction (an `Action` with an exact inverse, or a digest that ignores resources nothing in the arena references — decide and record in `document-model.md`).
- Undo of a placement and of a paste with bitmaps restores the canonical digest exactly; redo still finds the resource.
- `collect_unused` behaviour on save and history eviction unchanged.

## Notes
`SlotMap` cannot re-insert at a given key, so an inverse that removes and re-adds the resource needs care (tombstone, or keep the slot and hide it from the digest).
