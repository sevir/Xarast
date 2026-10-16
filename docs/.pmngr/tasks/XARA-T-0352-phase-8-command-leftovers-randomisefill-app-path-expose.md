---
id: XARA-T-0352
type: task
title: "Phase-8 command leftovers: RandomiseFill app path, expose CreateColour's new id, journal support for Fill/Palette"
status: backlog
priority: low
parent: XARA-US-0038
author: mcp
labels: [phase-8, app]
created: 2026-10-16T06:42:49Z
updated: 2026-10-16T06:42:49Z
---

## Description
From XARA-T-0212 (dd6e894..7c136fc): `RandomiseFill` has no app path/UI (see XARA-T-0341 H7); `PaletteCommand::Create` builds `CreateColour` inside `run` so callers read the new id back by name — expose it; add `EditCommand::Fill`/`Palette` to the operation journal once XARA-T-0217 lands (both are plain data).
