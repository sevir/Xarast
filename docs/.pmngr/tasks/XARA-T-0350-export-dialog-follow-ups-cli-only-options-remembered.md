---
id: XARA-T-0350
type: task
title: "Export dialog follow-ups: CLI-only options, remembered options per document (T11.1.5), preview, proxy level by output dpi, live portal check"
status: done
priority: medium
parent: XARA-US-0056
author: mcp
labels: [phase-11, ui, io]
created: 2026-10-16T05:30:05Z
updated: 2026-10-16T11:09:22Z
started: 2026-10-16T10:29:47Z
closed: 2026-10-16T11:09:22Z
---

## Description
From XARA-T-0192 (6240fba..e38d22d):
1. Dialog lacks CLI options: bleed, explicit rectangle, background colour, PNG depth/colour/interlace, JPEG progressive/subsampling, PDF and SVG options.
2. Remember export options per document (T11.1.5); preview.
3. A photo-adjusted bitmap's proxy level follows the fixed export view, not the output dpi (300 dpi export may use a reduced proxy): pass the output scale into the scene build.
4. Live xdg-desktop-portal file-chooser round trip on a real compositor not tried.
Also from XARA-T-0290: Replace decodes very large files on the UI thread (imports over 1 MiB already use a background job); optional keep-aspect on replace.
