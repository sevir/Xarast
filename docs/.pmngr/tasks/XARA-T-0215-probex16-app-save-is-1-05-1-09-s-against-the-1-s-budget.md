---
id: XARA-T-0215
type: task
title: ProbeX16 app save is ~1.05–1.09 s against the 1 s budget (snapshot restore cost)
status: in_progress
parent: XARA-US-0084
author: mcp
labels: [phase-6, perf, app-core]
created: 2026-09-23T19:55:07Z
updated: 2026-10-16T03:58:08Z
started: 2026-10-16T03:58:08Z
---

## Description
`cargo run --release -p xarast-app --example save_probe -- ProbeX16.xar /dev/shm/x` (tmpfs): UI thread 54 ms (snapshot); save thread 1.09 s with thumbnail / 1.05 s without = restore 126 ms + `xarast_format::save` 856 ms (serialise 507, package 327). The format alone is under budget; the app adds the snapshot→Document restore.

## Acceptance Criteria
- ProbeX16 File › Save ≤ 1 s wall on the save thread (tmpfs), UI-thread cost unchanged or lower.

## Notes
Options: serialise straight from `Snapshot` (the SVG writer only reads), or overlap restore with something; measure on tmpfs — the dev machine's 99 %-full md RAID adds 0.5–7 s of fsync.
