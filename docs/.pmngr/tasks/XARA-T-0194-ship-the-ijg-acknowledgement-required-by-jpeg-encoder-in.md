---
id: XARA-T-0194
type: task
title: Ship the IJG acknowledgement required by jpeg-encoder in the About box and AppImage notices
status: in_progress
priority: medium
author: mcp
labels: [packaging, licensing]
created: 2026-09-23T19:49:11Z
updated: 2026-10-15T19:32:00Z
started: 2026-10-15T19:32:00Z
---

## Description
JPEG export (`xarast-io`) uses `jpeg-encoder`, licensed `(MIT OR Apache-2.0) AND IJG`. The IJG licence's condition on a binary-only distribution is that the accompanying documentation state: "This software is based in part on the work of the Independent JPEG Group." The statement is recorded in `docs/11-licensing-and-clean-room.md` §4.1 and the exception is justified in `deny.toml`.

## Work
- Add a third-party notices file to the AppImage (and later the Windows/macOS packages) carrying every §4.1 statement verbatim.
- Show the same statements in Help › About.

## Acceptance Criteria
- A packaged build contains the IJG sentence in its bundled notices and in the About box.
