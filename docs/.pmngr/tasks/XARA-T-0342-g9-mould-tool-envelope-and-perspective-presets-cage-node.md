---
id: XARA-T-0342
type: task
title: "G9 — Mould tool: envelope and perspective presets, cage node handles, vanishing point, copy/paste mould, detach/remove, undo"
status: done
priority: high
parent: XARA-US-0071
author: mcp
labels: [phase-13, live-effects, tools, ui, app]
created: 2026-10-16T00:08:30Z
updated: 2026-10-16T00:50:15Z
started: 2026-10-16T00:08:30Z
closed: 2026-10-16T00:50:15Z
---

## Description
Phase-13 G9: edit the mould shape with node handles, envelope and perspective presets, draggable vanishing point, copy/paste envelope/perspective, detach/remove, grid toggle; J2 numeric access where meaningful. Builds on `EditCommand::Live`/`LiveEdit`, `Preview::live`/`RegenOverride` (XARA-T-0338, XARA-T-0340) and the mould maths (XARA-T-0334). Include hit-testing/snapping through the mould's inverse if needed for editing (part of XARA-T-0337).

## Acceptance Criteria
- Tool registered (in `AppCommand::ALL`); headless gesture/numeric tests; undo digest-exact; apply-then-remove identity; golden from scripted gestures; gates = CI.
