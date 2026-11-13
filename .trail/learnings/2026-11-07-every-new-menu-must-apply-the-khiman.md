---
title: "Every new menu must apply the menu look itself, and painted widgets need their own hand cursor"
author: Khiman
date: 2026-11-07
branch: master
cost: { minutes: 30, tokens: 151571 }
id: l-aaff2921
scope: repo
status: active
src: agent
touches:
  - src/app.rs
  - src/app/aggregate.rs
  - src/app/chrome.rs
  - src/app/compare.rs
  - src/app/documents.rs
  - src/app/import.rs
  - src/app/log.rs
  - src/app/modals.rs
  - src/app/relations.rs
  - src/app/schema.rs
---

## Decided
- Menu, context menu and dropdown items use 4 px corners via a helper called at the top of each menu closure. egui 0.37 gives every menu its own default style, built fresh, so there is no app-wide hook.
- The hand cursor comes from visuals.interact_cursor for egui Buttons only. Anything painted by hand (rows, cells, tabs, headers, checkboxes) must add on_hover_cursor(PointingHand) itself.
- Menus that can grow taller than the window wrap their content in the window-capped scroll helper; egui keeps popups inside the window and cuts the overflow off.

## Tried and ruled out
- A wrapper around context_menu/menu_button/ComboBox: it only moves the ~30 per-menu calls, so the plain call was kept.

## Watch out
- A new menu without the call gets the rounder 8 px button corners and looks out of place.

In the code: menu_look and menu_scroll in src/ui/widgets.rs (sticky_menu and row_menu already call menu_look); interact_cursor in visuals() in src/ui/theme.rs. QUARRY_TEST_POINTER=x,y,right with --screenshot captures an open context menu.
