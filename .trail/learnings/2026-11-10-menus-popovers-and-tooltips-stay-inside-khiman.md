---
title: "Menus, popovers and tooltips stay inside the app window"
author: Khiman
date: 2026-11-10
branch: master
cost: { minutes: 7, tokens: 39430 }
id: l-f3d8589a
scope: repo
status: active
src: agent
touches:
  - src/app/shell.rs
  - src/ui/widgets.rs
---

## Decided
Menus, popovers and tooltips are drawn inside the main window, as egui does by default. Letting them go past the window edge was considered and dropped.

## Tried and ruled out
- One borderless, transparent native window per popup (egui viewports placed in screen coordinates): egui's menus, popovers and tooltips cannot use one, so it needs a custom popup layer. Closing on an outside click, Esc and keyboard focus would have to work across windows, which matters for popovers with text fields. Opening also lags a frame or two, and Wayland does not let apps position windows.
- Native macOS context menus: they open outside the window, but lose the app's look and cannot hold text fields.

## Watch out
Tall menus already scroll instead of being cut off at the window edge.

In the code: src/ui/widgets.rs menu_scroll, sticky_menu, icon_menu; egui show_viewport_immediate and ViewportCommand::OuterPosition were the pieces looked at.
