---
title: Tried keeping the architecture and splitting big files rather than rewriting or adding abstractions
author: Khiman
date: 2026-11-07
standing: true
id: l-89385285
scope: repo
status: active
src: init-sessions
touches:
  - src/app.rs
  - src/db.rs
---

## Tried and ruled out
- The app works; the mess is file size. Hexagonal traits, a GUI framework switch and a Cargo workspace were rejected as unneeded for a single Mongo backend, with the workspace deferred until compile times hurt.
- In the code: Plan: split src/app.rs and src/db.rs into modules using multiple impl App blocks across files (ui/theme.rs, widgets, cells). Do not add repository traits or move to iced/gpui/Tauri.

_From past Claude sessions here, Nov 7._
