---
title: Right-click menus on whole rows need the container form or they never open
author: Khiman
date: 2026-11-09
branch: fix/schema-view-menu-scroll-fold
cost: { minutes: 6, tokens: 30680 }
id: l-0d38360c
scope: repo
status: active
src: agent
touches:
  - src/app/schema.rs
  - ".release-please-manifest.json"
  - CHANGELOG.md
  - Cargo.lock
  - Cargo.toml
  - src/app/config.rs
  - src/db.rs
  - src/db/analysis.rs
  - src/db/worker.rs
---

## Decided
A right-click menu on a row built with ui.horizontal must use container_context_menu. The row response only senses hover, so context_menu never sees the secondary click and the menu silently never opens; the schema field menu (Add as column, filters, map toggle) was dead this way.

## Watch out
Check any context_menu attached to a container response (ui.horizontal, ui.scope, ui.response) in egui 0.37.

In the code: schema_view in src/app/schema.rs.
