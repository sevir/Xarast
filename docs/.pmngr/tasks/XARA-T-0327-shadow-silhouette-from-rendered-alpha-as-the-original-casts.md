---
id: XARA-T-0327
type: task
title: Shadow silhouette from rendered alpha (as the original casts it) and feather-around-shadow in SVG export
status: backlog
priority: low
parent: XARA-US-0069
author: mcp
labels: [phase-13, live-effects, render, export]
created: 2026-10-15T16:10:02Z
updated: 2026-10-15T16:10:02Z
---

## Description
From XARA-T-0321 (6772dd6, render.md TODOs 33–34):
- A shadow's own silhouette ignores transparency (invariant 24), but the original casts its subtree's rendered alpha (`Kernel/bshadow.cpp:245`, `:277`), so a half-transparent object should get a half-dark shadow. Switching moves every shadow golden and the SoftShadow/Groucho2 scores: decide against the files' embedded previews.
- In SVG export, a feather filter around a shadowed object reads rendered alpha, not the outline rule. No corpus file has one.
