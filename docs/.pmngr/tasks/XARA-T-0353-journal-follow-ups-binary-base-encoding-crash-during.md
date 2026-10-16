---
id: XARA-T-0353
type: task
title: "Journal follow-ups: binary base encoding, crash during journal continuation, async close/save, history_mut bypass"
status: backlog
priority: low
parent: XARA-US-0026
author: mcp
labels: [phase-6, durability, perf]
created: 2026-10-16T08:40:54Z
updated: 2026-10-16T08:40:54Z
---

## Description
From XARA-T-0217 (…4d9daff): full `base.json.gz` at every autosave is costly on huge docs (ProbeX16 483 ms, 27.8 MiB; binary would ~halve); if continuing the journal fails at recovery, a second crash before the next base loses replayed ops; close/save wait on the UI thread for the writer's last fsync; code using `bus.history_mut()` bypasses the journal (only tests now); ProbeX16 replay of 2000 large-group moves takes 68 s. Also: 58/59 corpus docs change canonical digest on their first .xarast round trip (attributes regrouped) — stable after; worth recording/investigating.
