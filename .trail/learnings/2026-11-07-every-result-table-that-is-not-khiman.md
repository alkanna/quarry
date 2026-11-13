---
title: Every result table that is not Documents reads its rows from where its view keeps them
author: Khiman
date: 2026-11-07
branch: master
cost: { minutes: 7, tokens: 92261 }
id: l-a691c541
scope: repo
status: active
replaces: l-d21feb8d
src: agent
touches:
  - src/app/compare.rs
  - src/app/state.rs
  - src/app/documents.rs
  - src/app.rs
  - src/db/analysis.rs
  - src/ui/widgets.rs
---

## Decided
Aggregate, Shell, Import and Compare all draw through the same results table. The table asks the tab where the current view keeps its rows, which ones are on screen, and the first row number. It reads them in place. Each view used to copy its page into the Aggregate slot while the table drew, and every other place that needed those rows (the side panel, the Customize column dialog) had to be patched per view. Compare's differing-field rows are built on the worker together with the comparison, because diffing every changed document on the UI thread froze it.

## Tried and ruled out
Copying a page into the Aggregate slot and putting it back afterwards: the Customize dialog and the side panel then read the wrong rows, and column widths were measured every frame because the copy had a new address each time.

## Watch out
A new view with a result table only needs an arm in the row lookup, the row store and the page window, plus its own column-settings key. Rows from Compare and Import are not documents of the tab's collection: the field menu hides filter, group, distinct and create-index for them, and unsaved edits (drafts) must never be applied to them even when the _id matches. Result rows can repeat an _id, so the document tree's egui id includes the row.

In the code: Tab::agg_rows, agg_store, agg_window, agg_page and foreign_rows in src/app/state.rs; App::results in src/app/documents.rs; App::drop_agg_row and col_key in src/app.rs; Compared::diffs in src/db/analysis.rs; pager in src/ui/widgets.rs. QUARRY_DEMO=1 with QUARRY_TEST_COMPARE, QUARRY_TEST_IMPORT or QUARRY_TEST_SHELL opens each view with data.
