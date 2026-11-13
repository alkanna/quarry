---
title: Every result table that is not Documents reads its rows from where its view keeps them
author: Khiman
date: 2026-11-08
branch: refactor/split-app-rs
cost: { minutes: 1, tokens: 0 }
id: l-9462400c
scope: repo
status: active
replaces: l-6d05c278
src: agent
touches:
  - src/app/state.rs
  - src/app/documents.rs
  - src/app/tabs.rs
  - src/app/drafts.rs
---

## Decided
Aggregate, Shell, Import and Compare draw through the same results table, which asks the tab for the view's rows, columns, page and column-settings key and reads the rows in place. Compare's per-field diff rows are built on the worker with the comparison, because diffing on the UI thread froze it.

## Tried and ruled out
Copying each view's page into the Aggregate slot while the table drew: the Customize column dialog and the side panel then read the wrong rows, and widths were re-measured every frame.

## Watch out
A new result view needs an arm in both row lookups (read and mutable). Compare and Import rows are not documents of the tab's collection, so collection actions are hidden for them and unsaved edits must never apply to them, even with a matching _id.

In the code: Tab::agg_source, agg_store, agg_page and in_coll in src/app/state.rs; App::results in src/app/documents.rs; App::in_coll, col_key and drop_agg_row in src/app/tabs.rs; App::shown_doc and selected_doc in src/app/drafts.rs; Compared::diffs and DIFF_FIELDS in src/db/analysis.rs. QUARRY_DEMO=1 with QUARRY_TEST_COMPARE, _IMPORT or _SHELL opens each view with data.
