---
id: XARA-T-0310
type: task
title: Stop caught font-subsetter panics from writing crash reports
status: in_progress
parent: XARA-US-0064
author: mcp
labels: [phase-12, stability]
created: 2026-09-24T17:46:34Z
updated: 2026-10-15T19:44:23Z
started: 2026-10-15T19:44:23Z
---

## Description
`xarast_app::crash::expect_panics` marks a panic the caller catches, so the panic hook writes no report for it. The image decoder and the gallery thumbnailer use it. `xarast-text`'s embedding path (`embed/mod.rs`, `catch_unwind` around the subsetter) cannot, because `xarast-text` does not depend on `xarast-app`; a caught subsetter panic therefore leaves a spurious crash report.

## Acceptance Criteria
- Either the thread-local "expected panic" marker moves to a crate both can use, or the app wraps the call that reaches the subsetter in `expect_panics`.
- A test shows that a caught subsetter panic writes no report.

## Notes
Filed from XARA-US-0064 (F1).
