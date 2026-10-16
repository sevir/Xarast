---
id: XARA-T-0204
type: task
title: T8.1.5 — "No colour" as a first-class Colour value, distinct from transparent black
status: done
parent: XARA-US-0037
author: mcp
labels: [phase-8, color]
created: 2026-09-23T19:53:06Z
updated: 2026-10-16T12:06:51Z
started: 2026-10-16T06:42:49Z
closed: 2026-10-16T12:06:51Z
---

## Description
Not done in the W8.1 model round: today "no colour" is `Option<Colour>::None` in the `.xar` importer (`ColourRegistry::resolve`) and never reaches `xarast_color::Colour`. Adding `Colour::NONE` / `is_none()` means a new variant or sentinel matched in `xarast-format`, `xarast-app` (paint, picking) and `xarast-render` — a cross-crate change to coordinate with those owners.

## Notes
- Picking rule already exists: an interior is not hit-testable when the fill is none (`tools.md` decision 25).
- Palette edits can never produce "none" (the change journal relies on it; see `docs/memory/colour.md`).
