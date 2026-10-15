---
id: XARA-T-0309
type: task
title: Set XARAST_BUILD_ID in release and AppImage builds
status: done
parent: XARA-US-0064
author: mcp
labels: [phase-12, stability, packaging]
created: 2026-09-24T17:46:34Z
updated: 2026-10-15T19:44:14Z
started: 2026-10-15T19:32:00Z
closed: 2026-10-15T19:44:14Z
---

## Description
Crash reports name the build as `CARGO_PKG_VERSION` plus `option_env!("XARAST_BUILD_ID")` (`xarast_app::crash::build_id`). Nothing sets `XARAST_BUILD_ID` yet, so every report of a given version looks the same.

## Acceptance Criteria
- The release workflow and the AppImage build export `XARAST_BUILD_ID` (the commit, short hash) when compiling `xarast`.
- The reproducible-build job still produces identical bytes (the id must not include a timestamp).
- `xarast --version --verbose` prints it too.

## Notes
Filed from XARA-US-0064 (F1).
