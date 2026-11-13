---
title: Sticky menus close when a dialog opens over the page
author: Khiman
date: 2026-11-10
branch: master
cost: { minutes: 24, tokens: 163696 }
id: l-5f122430
scope: repo
status: active
src: agent
touches:
  - src/app/palette.rs
  - src/ui/theme.rs
  - src/app.rs
  - src/ui/widgets.rs
  - src/app/chrome.rs
---

## Decided
Every sticky menu closes as soon as a dialog or window covers the page (the same test as window_open). The tab list takes arrows, Enter and focus while open, so a Cmd+K palette opened over it lost its keys to the hidden list. Fixing it in sticky_menu covers Columns, Library, Packages and any new sticky menu too.

## Tried and ruled out
A guard inside the tab list only: the other sticky menus kept the stale popup, and the next new one would repeat the bug.

## Watch out
keys() runs after the tab strip draws, so a dialog opened by a shortcut is seen by sticky menus on the next frame. The flag is set at the start of each frame from window_open.

In the code: set_window_over_page in src/app.rs ui(), read in sticky_menu in src/ui/widgets.rs.
