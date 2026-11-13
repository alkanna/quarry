---
title: Settings files are saved by writing a temp file then renaming
author: Khiman
date: 2026-11-07
standing: true
id: l-3a9e8cda
scope: repo
status: active
src: init-review
---

## Decided
- Rewriting settings in place meant a crash mid-save could leave a truncated file that failed to parse and got set aside.
- In the code: For ui.json and connections.json, write a temp file in the same folder and rename it over the old one. Also copy connections.json to connections.json.bak before a save that drops entries.

_From PRs #82, #80 here, Nov 6._
