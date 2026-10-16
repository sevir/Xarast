---
id: XARA-T-0088
type: task
title: F6.8/F6.9 — xarast repair (local-header scan) and partial XML recovery
status: done
parent: XARA-US-0026
author: mcp
labels: [phase-6, xarast-format, durability]
created: 2026-09-23T14:23:38Z
updated: 2026-10-16T06:19:54Z
started: 2026-10-16T05:30:19Z
closed: 2026-10-16T06:19:54Z
---

## Description
`xarast repair`: when the end record / central directory fails the reader's pre-check, scan `PK\3\4` local headers, validate against the manifest digests where readable, rebuild with `PackageWriter`. Malformed document.svg: parse up to the error, open read-only with a warning; corrupt resources become visible placeholders.

## Notes
Left over from round 1. The reader's open path is strict by design (see docs/memory/xarast-format.md), so repair is a separate path.
