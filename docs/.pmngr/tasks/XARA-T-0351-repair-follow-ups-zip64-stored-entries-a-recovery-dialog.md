---
id: XARA-T-0351
type: task
title: "Repair follow-ups: ZIP64 STORED entries, a recovery dialog, keep manifest foreign data"
status: backlog
priority: low
parent: XARA-US-0026
author: mcp
labels: [phase-6, xarast-format, durability]
created: 2026-10-16T06:19:59Z
updated: 2026-10-16T06:19:59Z
---

## Description
From XARA-T-0088 (dd68873..7442bf1): STORED entries with ZIP64 sizes aren't recovered (DEFLATE are); the app shows only a generic "Damaged file" notice plus per-image warnings, no recovery dialog; repair loses the manifest's foreign extension data and refcounts.
