---
title: Fold requests must survive into the next frame or Expand does nothing
author: Khiman
date: 2026-11-07
standing: true
id: l-470ad00d
scope: repo
status: active
src: init-sessions
touches:
  - src/app.rs
---

## Watch out
- Expand recursively, Expand all and Collapse all did nothing because the request was set after headers drew and cleared at the end of the same frame. An altitude reviewer suggested clearing earlier, which would reintroduce the bug.
- In the code: In src/app.rs the fold menu sets fold_busy = true and sends TreeAct::Fold, applied after the end-of-document clear. Do not move the fold clear before the tree loop; it would read last frame's fold_busy (false).

_From past Claude sessions here, Nov 7._
