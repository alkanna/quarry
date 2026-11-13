---
title: "SQL actions that repeat the grid's query must use the query of the last page, not the bar text"
author: Khiman
date: 2026-11-13
branch: fix/sql-column-data
cost: { minutes: 1, tokens: 25282 }
id: l-e0cb217f
scope: repo
status: active
src: agent
touches:
  - src/app/column_menu.rs
  - src/app/modals.rs
  - src/app/sql/ns.rs
  - src/app/sql/views.rs
  - src/app/state.rs
  - src/app/table.rs
  - src/db/sql/value.rs
  - src/db/sql/worker.rs
  - src/db/analysis.rs
---

## Decided
- The SQL Content bar has no "applied" query: its texts change as the user types, before Enter. Anything that must match what the grid shows (whole-query column actions) uses the query saved when the last page was asked for.
- Whole-query column reads skip the page's COUNT, so a slow count on a big filtered table cannot fail the action.
- DECIMAL sums use f64, so 0.1 + 0.2 shows 0.30000000000000004, the same as Mongo doubles. Exact decimal sums were left for the user to decide.

## Watch out
- A SQL tab's Shell results are documents, so they read dotted paths like Mongo results. Only the Content grid and Query results are SQL rows with column-name keys.

In the code: SqlTab.ran, set in reload_sql (src/app/sql/views.rs); page(.., counted) in src/db/sql/worker.rs; col_data in src/app/column_menu.rs.
