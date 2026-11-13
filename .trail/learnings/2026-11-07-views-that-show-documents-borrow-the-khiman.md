---
title: Views that show documents borrow the shared results table instead of drawing their own rows
author: Khiman
date: 2026-11-07
branch: master
cost: { minutes: 4, tokens: 121505 }
id: l-d21feb8d
scope: repo
status: active
src: agent
touches:
  - src/app/state.rs
  - src/app/documents.rs
  - src/app/compare.rs
  - src/app/shell.rs
  - src/app/import.rs
  - src/app.rs
---

## Decided
Compare, Shell and Import show their rows through the same results table as Documents (table or JSON, paging, column options, rows that open inline). They lend one page of rows to the aggregate slot only while the table draws, then take them back. Compare lists differing fields as one row each (_id, field, target, source) so they fit the same table.

## Tried and ruled out
Compare's own collapsing lists of one-line JSON: they had no paging, stopped at 200 rows and looked different from every other view.

## Watch out
A view that lends rows also needs three other places updated, or things quietly break: the column-settings key (otherwise it shares widths with Aggregate), the row-number base for paging, and the selected-row lookup for the side panel (the lent rows are gone once the table finishes drawing). Keep the lent page cached between frames: column widths are cached by the rows' memory address, so copying a fresh page every frame measures every column every frame.

In the code: App::results(ui, true) in src/app/documents.rs; lenders in src/app/shell.rs, src/app/import.rs, src/app/compare.rs; col_key and selected_doc in src/app.rs; base match in results(); CompareTab.shown caches the page; QUARRY_DEMO=1 QUARRY_TEST_COMPARE=1 opens a finished comparison.
