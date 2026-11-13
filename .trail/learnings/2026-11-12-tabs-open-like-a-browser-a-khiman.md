---
title: "Tabs open like a browser: a click reuses the current tab unless it holds work, new tabs are asked for"
author: Khiman
date: 2026-11-12
branch: feat/browser-tabs
cost: { minutes: 1, tokens: 0 }
id: l-38ebbdf6
scope: repo
status: active
src: agent
touches:
  - src/app/tabs.rs
  - src/app/sidebar.rs
  - src/app/palette.rs
  - src/app/chrome.rs
  - src/menu.rs
---

## Decided
- A plain click on a collection (tree, Cmd+K palette, a table just created) reuses the current tab, unless that tab holds work. In that case it opens a new tab right after the current one. A tab already showing the collection is switched to.
- Work means one of these:
  - A workspace view: Shell, Query, Import, Compare or Watch.
  - A filter, sort or SQL bar clause, a projection, or an aggregation pipeline.
  - Staged drafts for that collection.
  - A cell or document editor that is open.
- The replaced tab goes on the closed-tabs list, so Cmd+Shift+T brings it back.
- New tabs on request:
  - Cmd+click, middle-click, or right-click > "Open in new tab" in the tree.
  - Cmd+Enter, Cmd+click or middle-click in the palette.
  - Cmd+T, File > New Tab, and the tab strip's "+" open the palette in new-tab mode. Duplicate lives in the tab's right-click menu.
- Links inside data (SQL foreign-key cell links, Mongo "Go to referenced document") open a new tab, because there is no per-tab Back to return to the rows they came from.

## Tried and ruled out
- Per-tab Back/Forward history, as a browser has. The user asked to leave it out. It would allow links to replace the current tab too.
- Links that replace a clean current tab: without Back, the source rows, page and scroll would be lost.
- A right-click menu on palette rows: the palette is driven from the keyboard, so Cmd+Enter is the gesture there.

## Watch out
- open_collection takes Open::Here, Open::NewTab or Open::Replace. Replace is only for the Shell switching its own collection, which must happen even though the Shell tab holds work.
- The work check runs before reset_doc_state, which closes the document editor it looks at.

In the code: src/app/tabs.rs (open_collection, keeps_work, has_clauses), src/app/sidebar.rs tree row clicks, src/app/palette.rs (new_tab on Modal::Palette, open_new_tab), src/app/chrome.rs (Cmd+T and the "+" button), src/menu.rs (New Tab…), test src/app/ui_tests/tabs.rs (offline, demo data).
