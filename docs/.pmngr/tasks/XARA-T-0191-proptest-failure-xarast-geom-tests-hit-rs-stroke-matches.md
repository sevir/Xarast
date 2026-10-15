---
id: XARA-T-0191
type: task
title: "Proptest failure: xarast-geom tests/hit.rs stroke_matches_the_renderers_outline"
status: done
author: mcp
labels: [phase-7, geometry, bug]
created: 2026-09-23T19:29:14Z
updated: 2026-10-15T20:20:20Z
started: 2026-10-15T20:02:49Z
closed: 2026-10-15T20:20:20Z
---

## Description
Found on the integration branch at 5b21693 (not caused by W7/W8 changes, which only add functions to measure.rs). Random proptest case; shrinks to: path = MoveTo(-1491,-6354) CubicTo((1236,9796),(8540,-5241),(-3209,-1484)), identity matrix, width 1903, caps 0, join 0 (mitre), mitre 1.0, not dashed, point (3684,-2160), r = 406.1087. Seed: `cc 1534a3f812d777f48d27fb7a81a0a515ebff48165181784fa3fb619fe16e76ca`. Also seen once flaky under load: xarast-app render_thread::tests::cancel_drops_the_waiting_frame_and_the_one_in_flight (passes alone).

## Acceptance Criteria
Seed added to hit.proptest-regressions and passing.
