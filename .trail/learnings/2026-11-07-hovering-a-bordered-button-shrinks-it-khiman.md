---
title: "Hovering a bordered button shrinks it and shifts neighbors, so chips are custom drawn"
author: Khiman
date: 2026-11-07
standing: true
id: l-96c0d252
scope: repo
status: active
src: init-sessions
touches:
  - src/app.rs
---

## Watch out
- egui buttons with a custom border lose 1px when hovered, which made tags jump around. Chips and the sidebar tag badge now share one fixed-size widget.
- In the code: Use the shared chip() widget (custom painted, fixed geometry) for tags, pills and toggle chips in src/app.rs. Do not build chips from egui Button with stroke. pill() was merged into chip(ui, ..., true).

_From past Claude sessions here, Nov 7._
