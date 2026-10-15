---
id: XARA-T-0340
type: task
title: "E6, F8 — Contour and Blend tools: drag to create, infobars with numeric fields, edit ends live, remove, undo"
status: in_progress
priority: high
parent: XARA-US-0070
author: mcp
labels: [phase-13, live-effects, tools, ui, app]
created: 2026-10-15T23:20:43Z
updated: 2026-10-15T23:20:43Z
started: 2026-10-15T23:20:43Z
---

## Description
Phase-13 rows E6 (contour tool + infobar: steps, width, inner/outer, join, object profile, attribute profile) and F8 (blend tool: drag object to object, steps/distance in the infobar, attach/detach path if F6 lands, edit end objects live, remove blend), plus J2 numeric access. Build on XARA-T-0338's `EditCommand::Live`, `Tx::set_live_kind`, `Preview::live` and the shadow/bevel tool architecture. Contours and blends regenerate geometry: one regeneration per drag frame, regeneration never an undo step (A11) — this also covers the remaining half of XARA-T-0316.

## Acceptance Criteria
- Tools registered; headless gesture/numeric tests; undo digest-exact; apply-then-remove identity; goldens from scripted gestures; gates = CI.
