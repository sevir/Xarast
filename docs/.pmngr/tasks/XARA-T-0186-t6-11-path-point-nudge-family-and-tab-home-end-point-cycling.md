---
id: XARA-T-0186
type: task
title: T6.11 Path-point nudge family (and Tab/Home/End point cycling)
status: done
parent: XARA-US-0034
author: mcp
labels: [phase-7, tools]
created: 2026-09-23T19:00:17Z
updated: 2026-10-16T06:21:59Z
started: 2026-10-16T05:30:19Z
closed: 2026-10-16T06:21:59Z
---

## Description
Leftover of US-0034: arrow-key nudges of selected path points (depends on the W4 nudge family, T4.10), Tab/Shift+Tab and Home/End point cycling (research/04 §4.11). SetPath with PathEdit::Move already coalesces per path for nudge runs.

## Acceptance Criteria
- The 24 nudge variants move selected points; a run of nudges is one undo step.
