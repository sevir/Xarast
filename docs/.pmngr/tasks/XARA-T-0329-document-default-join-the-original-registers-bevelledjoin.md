---
id: XARA-T-0329
type: task
title: "Document default join: the original registers BevelledJoin, Xarast defaults to Mitre"
status: done
priority: medium
author: mcp
labels: [document-model, render, fidelity]
created: 2026-10-15T17:09:40Z
updated: 2026-10-15T20:25:53Z
started: 2026-10-15T19:00:23Z
closed: 2026-10-15T20:25:53Z
---

## Description
Found during XARA-T-0326: the original registers `BevelledJoin` as the document default join (`Kernel/attr.cpp:1967-1980`), while Xarast defaults to `Mitre`. 21 of 22 corpus bevel controllers carry no join and so use the default, and the default applies to every stroke corner with no explicit join. Verify against the corpus (does any file rely on the default for strokes?), decide, and change the default if confirmed; expect golden and export-score moves. See document-model.md TODO.

## Acceptance Criteria
- Decision recorded in document-model.md with evidence; goldens/limits updated with reasons.
