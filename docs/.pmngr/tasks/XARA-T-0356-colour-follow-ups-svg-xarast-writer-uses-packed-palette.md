---
id: XARA-T-0356
type: task
title: "Colour follow-ups: SVG/.xarast writer uses packed palette quantisation, transparent literal vs none, no-colour in the colour editor"
status: backlog
priority: low
parent: XARA-US-0037
author: mcp
labels: [phase-8, color, format]
created: 2026-10-16T12:06:51Z
updated: 2026-10-16T12:06:51Z
---

## Description
From XARA-T-0203/0204 (9362638..9aa6acf): the SVG/.xarast writer still rounds colours, so a palette colour exports 1 level off its cached RGB in the packing band (switching also changes `resolve_rgba8` in the reader and .xarast bytes); a literal fully transparent flat colour is written `none` and reloads as `Colour::None`; the colour editor can't set "no colour" and shows it as transparent black. Corpus PNGs moved (14/59 files, 94 % of changed pixels by 1 level; up to 19 levels in ProbeX16 — unverified amplification through HSV ramps/transparency).
