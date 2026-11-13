---
title: Filled buttons never showed hover or pressed states until given their own shading
author: Khiman
date: 2026-11-07
standing: true
id: l-f78c3cef
scope: repo
status: active
src: init-sessions
touches:
  - src/app.rs
---

## Watch out
- In egui a button with its own fill uses that fill for every state, so primary buttons, pills and segmented controls looked static. A helper now shades the fill by state.
- In the code: Use state_fill() in src/app.rs for any button with a custom fill. QUARRY_TEST_POINTER places the pointer in screenshot mode to verify hover visually.

_From past Claude sessions here, Nov 5._
