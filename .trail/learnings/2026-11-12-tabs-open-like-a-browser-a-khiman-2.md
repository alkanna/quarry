---
title: "Tabs open like a browser: a click reuses the current tab unless it holds work, new tabs are asked for"
author: Khiman
date: 2026-11-12
branch: feat/browser-tabs
cost: { minutes: 2, tokens: 32723 }
id: l-3cdcc6e3
scope: repo
status: active
replaces: l-38ebbdf6
src: agent
touches:
  - src/app.rs
  - src/app/chrome.rs
  - src/app/demo.rs
  - src/app/errors.rs
  - src/app/palette.rs
  - src/app/relations.rs
  - src/app/run.rs
  - src/app/sidebar.rs
  - src/app/sql/create.rs
  - src/app/sql/create_pg.rs
---

## Decided
- A plain click on a collection reuses the current tab, unless that tab holds work. If it does, the collection opens in a new tab next to the current one. A tab already showing the collection is switched to. A replaced tab can be reopened with Cmd+Shift+T.
- Work means any of these:
  - A workspace view: Shell, Query, Import, Compare or Watch.
  - Text the person typed in any view of the tab, not only the one shown.
  - Staged drafts, or staged structure or trigger changes, or an ALTER that is running.
  - An open value, cell or document editor.
- New tabs on request: Cmd+click, middle-click or right-click > "Open in new tab" in the tree. Cmd+Enter in the palette. Cmd+T or the tab strip's "+", which open the palette in new-tab mode. A pick there that is already open is switched to.
- Links inside data (foreign-key cells, referenced documents, Structure and Relations table links) always open a new tab, because there is no per-tab Back.

## Tried and ruled out
- Per-tab Back/Forward: the user asked to leave it out. It is what would let links replace the tab too.
- Judging work by the shown view only: a Shell script or pipeline in a tab switched back to Documents was lost on the next click.

## Watch out
- One rule decides reuse for clicks and foreign-key links. Put new kinds of tab state there, or a click silently replaces a tab that holds it.
- The check runs before reset_doc_state, which closes the editors it looks at.

In the code: src/app/tabs.rs (open_collection with Open::Here, NewTab or Replace; keeps_work; new_tab_click), NEW_PIPELINE in state.rs, palette new_tab mode in palette.rs, test src/app/ui_tests/tabs.rs.
