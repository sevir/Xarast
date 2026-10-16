---
id: XARA-T-0217
type: task
title: F6.6 — Operation journal (NDJSON) between autosaves
status: done
parent: XARA-US-0026
author: mcp
labels: [phase-6, durability]
created: 2026-09-23T19:55:07Z
updated: 2026-10-16T08:40:54Z
started: 2026-10-16T06:19:59Z
closed: 2026-10-16T08:40:54Z
---

## Description
Split out of XARA-T-0087: autosave snapshots and recovery are done (xarast-app `autosave`), the append-only journal is not. It needs a serialisable form of every document command (xarast-doc), fsync batching ≤ 250 ms, truncation on autosave and seq-continuity-checked replay on recovery.
