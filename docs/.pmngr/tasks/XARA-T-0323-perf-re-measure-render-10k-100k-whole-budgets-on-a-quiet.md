---
id: XARA-T-0323
type: task
title: "perf: re-measure render-10k/100k --whole budgets on a quiet machine and tighten"
status: backlog
priority: low
parent: XARA-US-0061
author: mcp
labels: [perf]
created: 2026-10-15T12:38:55Z
updated: 2026-10-15T12:38:55Z
---

## Description
XARA-T-0319 (70f3c2a) re-based render-10k/render-100k on `bench render --whole` with references 13.4 ms / 108 ms, measured at load 10–27 while other agents built. Re-measure on a quiet reference machine (more runs; the first cold frame is inside the 7-frame median) and tighten limits if numbers drop.
