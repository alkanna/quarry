---
title: "Dark and light themes share one layout and differ only in colors, with popups using their own hover color"
author: Khiman
date: 2026-11-08
branch: master
cost: { minutes: 21, tokens: 156620 }
id: l-06829fb0
scope: repo
status: active
src: agent
touches:
  - THIRD_PARTY_NOTICES.md
  - assets/fonts/Inter-OFL.txt
  - assets/fonts/InterVariable.ttf
  - assets/fonts/JetBrainsMono-OFL.txt
  - assets/fonts/JetBrainsMono-Variable.ttf
  - docs/aggregate.png
  - docs/compare.png
  - docs/connection.png
  - docs/documents.png
  - docs/edit.png
---

## Decided
- The dark theme (Workbench) and the light theme (Paper) use the same layout: rail, tab strip, query card and table. Light mode only swaps colors. The user chose this over giving light mode its own chrome details.
- Every interactive element follows one state model: idle, hovered, pressed and focused. Frameless controls use the HOVER fill on hover and PRESSED while held.
- Menus, the autocomplete list and the column popover sit on the raised popup color. The page HOVER fill does not show there, so they use MENU_HOVER and a pressed variant of it.

## Watch out
- Any new window drawn over the page must be added to the "is a window open" check. Otherwise Esc and page shortcuts reach the page behind the window.
- The "/" shortcut does not type a slash into the collection filter. The sidebar is drawn before the key handler runs, so the filter is not focused yet when that keypress arrives. A review flagged this as a bug; it is not one.

In the code: src/ui/theme.rs (Palette, hover_fill, menu_pressed, next_state), src/ui/widgets.rs (state_of, quiet_look, menu_look), src/app/chrome.rs (window_open, keys).
