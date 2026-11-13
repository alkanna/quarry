---
title: Theme spacing must be applied to both light and dark styles
author: Khiman
date: 2026-11-07
standing: true
id: l-98ba4f38
scope: repo
status: active
src: init-sessions
touches:
  - src/app.rs
---

## Watch out
- Spacing tweaks were only applied to the active theme at startup, so switching theme made settings content jump. Unchecked checkboxes also had no border and were invisible on cards.
- In the code: Use all_styles_mut, not global_style_mut, when setting spacing/visuals in the theme setup in src/app.rs. Check both themes with screenshots.

_From past Claude sessions here, Nov 5._
