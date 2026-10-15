---
id: XARA-T-0325
type: task
title: D gate — Determine bevel cross-sections, lighting curve and inner-bevel style swap against the original in a VM
status: backlog
priority: medium
parent: XARA-US-0069
author: mcp
labels: [phase-13, live-effects, render, vm]
created: 2026-10-15T13:50:43Z
updated: 2026-10-15T13:50:43Z
---

## Description
XARA-T-0322 (f7798ca..6204754) shipped Xarast's own cross-sections and `shade()` rule for all 15 bevel styles; the original's curves live in closed CDraw. Determine them empirically on a VM running the original (blocked with XARA-US-0018 on the VM), and the 180° style swap for some inner bevels. render.md TODO 30.
