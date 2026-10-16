---
id: XARA-T-0212
type: task
title: App wiring — EditCommand variants for the phase-8 fill and palette commands
status: done
parent: XARA-US-0038
author: mcp
labels: [phase-8, app, tools]
created: 2026-09-23T19:53:36Z
updated: 2026-10-16T06:42:43Z
started: 2026-10-16T06:21:59Z
closed: 2026-10-16T06:42:43Z
---

## Description
The commands exist in `xarast-doc` (`fill_edit`, `palette`, all implementing `xarast_doc::Command`). `xarast-app`'s `EditCommand` needs variants (or a pass-through) so tools and the UI can dispatch them through `Session::apply_edit`.

## Contract
- Fill commands: `SetFillGeometry`, `MoveFillControl`, `InsertStop`, `MoveStop`, `RemoveStop`, `SetStopValue`, `SetFillProfile`, `SetRampMapping`, `SetFillEffect`, `SetTiling`, `SetTranspMode`. Each names `(node, PaintSlot, FillChannel)`; they fail with `EditError::FillEdit(reason)` without touching the document when the handle/stop/profile does not exist.
- Drags: pass the bus gesture as `drag: Some(g)` on `MoveFillControl` / `MoveStop`; their own `CoalesceKey` merges one drag of one handle into one step. `ramp_move` returns a stop's new index after a re-sort, for the tool to keep dragging the same stop.
- Palette: `CreateColour` (read the new id from `created.get()` after dispatch), `RedefineColour`, `RenameColour`, `ReparentColour`, `DeleteColour { policy }` → `EditError::Palette(ColourEditError)` on refusal. After a palette command repaint `ColourUses::users_of(changed_between(before, after))`.
- `xarast_doc::fill_edit::set_own_attr` is the same rule as `ops.rs`'s private helper; switch to it.
- Undo labels: "Set Fill", "Set Transparency", "Move Fill Handle", "Add/Move/Delete Fill Stop", "Set Fill Colour", "Fill Profile", "Fill Mapping", "Fill Effect", "Fill Tiling", "Transparency Type", "Create/Edit/Rename/Delete Colour", "Link Colour".
