---
title: "Custom-painted labels center the visible letters, and text is laid out through the painter"
author: Khiman
date: 2026-11-07
branch: master
cost: { minutes: 5, tokens: 56814 }
id: l-73c2fa03
scope: repo
status: active
src: agent
touches:
  - src/ui/complete.rs
  - src/ui/theme.rs
  - src/ui/widgets.rs
---

## Decided
Chips, segmented controls and the autocomplete popup paint their own text. It is centered on the ink (tallest letter to baseline), not the line box, because box centering leaves descender room and all-caps labels sat high. Segmented controls are painted at BUTTON_H instead of wrapped in a Frame, which grew with the row height.

## Watch out
In egui 0.37, ui.fonts(|f| f.layout_...) no longer compiles because the closure gets a shared reference. Lay out text with ui.painter().layout_no_wrap / layout_job instead.

In the code: optical_dy in src/ui/theme.rs; segmented and paint_chip in src/ui/widgets.rs; popup_list in src/ui/complete.rs.
