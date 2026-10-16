---
id: XARA-T-0203
type: task
title: T8.1.4 (walker) — Resolve Colour::Indexed through PaletteResolver and key caches on the palette epoch
status: in_progress
parent: XARA-US-0037
author: mcp
labels: [phase-8, app, render]
created: 2026-09-23T19:53:06Z
updated: 2026-10-16T06:42:49Z
started: 2026-10-16T06:42:49Z
---

## Description
The doc side is done (XARA-T-0198). The scene walker in `xarast-app` (`paint.rs` `rgba()` / `colour_stops()`, `walker.rs` scope) still calls `Colour::resolve(table).to_rgba8()`.

## Contract
- Hold one `xarast_doc::PaletteResolver` per session/walker and call `resolver.resolve(&colour, &doc.resources.colours) -> Rgba8` for every colour painted (fill endpoints, ramp stops, three/four-colour corners, stroke colour). It memoises per palette entry and clears itself when `ColourTable::epoch()` changes.
- It quantises with `ColourValue::to_rgba8_packed` (the original's FIXED24 packing), so a flat palette colour paints exactly the file's cached RGB. Direct colours also move in a narrow band (`k + 0.5 … k + 0.502` in 255ths): expect a handful of 1-LSB golden diffs; regenerate them.
- Fold `doc.palette_epoch()` into every render `CacheKey` that depends on resolved colours (today `resources_rev` is bumped by `Action::SetPalette`, which is correct but coarser).
- Dirty region after a palette command: `let changed = xarast_doc::palette::changed_between(&before, &doc.resources.colours); ColourUses::users_of(&changed)` gives the owner nodes whose bounds to union (acceptance 11). Build `ColourUses` on load (`ColourUses::build(&doc)`) and after structural edits until it is maintained incrementally.
- A palette swap does **not** write the tree change journal (the pick index needs nothing).

## Acceptance Criteria
- Redefining a palette entry used by 5 000 objects repaints all of them; dirty rect = union of their bounds.
- Golden renders unchanged except the documented 1-LSB packing band.
