---
title: Do not add border strokes to resizable side panels; the resize edge already draws one
author: Khiman
date: 2026-11-07
standing: true
id: l-495899be
scope: repo
status: active
src: init-sessions
touches:
  - src/app.rs
---

## Watch out
- The sidebar and inspector had their own 1px border plus egui's resize line, making borders look twice as thick as elsewhere.
- In the code: Leave sidebar and inspector panel frames without a stroke in src/app.rs; egui draws a 1px line on the resize edge.

_From past Claude sessions here, Nov 5._
