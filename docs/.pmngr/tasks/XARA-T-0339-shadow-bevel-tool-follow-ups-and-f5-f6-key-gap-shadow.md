---
id: XARA-T-0339
type: task
title: "Shadow/bevel tool follow-ups and F5/F6 key gap: shadow colour, second shadow and 1–4/A keys, contoured bevel drag outline, wall-offset nudges, Fill/Transparency tools missing from AppCommand::ALL"
status: in_progress
priority: medium
parent: XARA-US-0069
author: mcp
labels: [phase-13, tools, app, ui]
created: 2026-10-15T23:20:43Z
updated: 2026-10-15T23:21:58Z
started: 2026-10-15T23:21:58Z
---

## Description
From XARA-T-0338 (c1e25a1..0d852e8):
1. Shadow colour not editable (no infobar colour item; colour bar doesn't target shadows).
2. Original's Shift-drag second shadow and plain keys 1–4/A (keys 1 and 3 are zoom here: decide).
3. Bevel size drag outlines the box, not the contoured path; no snapping.
4. Arrow-key nudges for a wall shadow's offset.
5. **Pre-existing bug:** F5/F6 (Fill and Transparency tools) are not in `AppCommand::ALL`, so the shell never binds those keys.
6. Draft-quality effects during drags need a render-crate change (fits XARA-T-0315).
