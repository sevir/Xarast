---
id: XARA-T-0189
type: task
title: Snapping for shape-editor nodes, pen points and guide drags; guide properties and Delete all guides
status: in_progress
parent: XARA-US-0036
author: mcp
labels: [phase-7, tools]
created: 2026-09-23T19:29:14Z
updated: 2026-10-16T09:13:40Z
started: 2026-10-16T09:13:40Z
---

## Description
`ToolCtx::snap_point` is applied to move, scale, rotation centre and rectangle/ellipse creation. The path tools (node_edit, pen, freehand start) and guide drags from the rulers do not snap yet (T8.7, acceptance 12 "node editing"). Guide properties dialog and a Delete all guides menu item (`GuideOp::DeleteAll` exists) are missing (T8.6).

## Acceptance Criteria
Mid-drag NumPad . snaps a node drag; guide drag snaps to grid; menu items present.
