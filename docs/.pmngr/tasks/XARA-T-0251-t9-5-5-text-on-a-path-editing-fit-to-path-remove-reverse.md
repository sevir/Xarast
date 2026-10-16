---
id: XARA-T-0251
type: task
title: "T9.5.5 Text on a path editing: fit to path, remove, reverse, start offset"
status: done
parent: XARA-US-0048
author: mcp
labels: [phase-9, text]
created: 2026-09-23T23:43:49Z
updated: 2026-10-16T16:16:30Z
started: 2026-10-16T15:21:27Z
closed: 2026-10-16T16:16:30Z
---

## Description
W9.5 draws imported text on a path; nothing creates or edits it yet. Add the commands: fit a story to a selected path (the path becomes the story's first child; the story matrix is decomposed into a translation plus `CharsTransform`, text rotated past ±90° is reversed instead), remove from the path (the inverse), reverse along the path (Ctrl+Shift+R), and drag the left/right indents; the path stays editable as a child node. Facts in `docs/memory/text.md` "Text on a path" (MatrixFitToPath / MatrixRemoveFromPath).

## Acceptance Criteria
- Each command is one undo step and undoes exactly (canonical digest).
- Fit then remove restores the story's render.
- Reverse on `TextCurve.xar` flips the text to the other end and side as the flags say.
- Word wrap on a path is decided (model flag or documented non-goal).
