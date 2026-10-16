---
id: XARA-T-0316
type: task
title: A4 wiring — flush RegenQueue before paint and draw LiveOutput::Shapes
status: done
priority: medium
parent: XARA-US-0068
author: mcp
labels: [phase-13, live-effects, app]
created: 2026-09-24T18:06:47Z
updated: 2026-10-16T00:08:30Z
closed: 2026-10-16T00:08:30Z
---

## Description
XARA-US-0068 built `xarast_doc::regen` (`regenerate`, `RegenKey`, `LiveCache`, `RegenQueue`) but did not wire it into the session: no generator exists yet (every kind returns `LiveOutput::Stored`), so wiring would add cost with no visible effect. Land it with the first geometry generator (contour or blend, XARA-US-0070; mould, XARA-US-0071).

## Acceptance Criteria
- `Session` owns a `RegenQueue` and a `LiveCache`; the change journal it already drains for the picker also feeds `RegenQueue::mark_changes`; `flush` runs once before `rebuild_scene` at the view's dpi; `flush_urgent` runs before export.
- The walker draws `LiveOutput::Shapes` in place of the stored generated subtree, and `live_pending` counts only controllers whose output is `Stored` and whose kind has a generator.
- A drag of a live parameter regenerates once per frame, one undo step per gesture (test), and the render thread's scene damage covers the old and the new extent (test against a full frame).

## Notes
`docs/memory/document-model.md` decisions 41–45.
