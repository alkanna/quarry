---
title: "Column Data actions on SQL tables read DECIMAL and dates as numbers only for the stats, and never treat column names as paths"
author: Khiman
date: 2026-11-13
branch: fix/sql-column-data
cost: { minutes: 8, tokens: 170021 }
id: l-96a23ded
scope: repo
status: active
src: agent
touches:
  - src/app/column_menu.rs
  - src/app/modals.rs
  - src/app/poll.rs
  - src/db.rs
  - src/db/analysis.rs
  - src/db/sql/tests/pg_read.rs
  - src/db/sql/value.rs
  - src/db/sql/worker.rs
  - src/db/worker.rs
---

## Decided
- The column popover's Data submenu now works on MySQL, MariaDB and PostgreSQL grids, page and whole-query scope.
- SQL rows keep DECIMAL, DATE and DATETIME/timestamp values as text so they stay exact. Sum, average, median and range convert them to numbers and dates first; copies, count distinct and group keep the exact text.
- A value with no zone is read as UTC, which keeps spans and order right. PostgreSQL zones like +00 are padded to +00:00 before parsing.
- Whole-query on SQL works like the Mongo path: it ignores the LIMIT and OFFSET chips, stops at COL_VALUES_CAP rows, and reads only that column when the bar selects every column.
- Group results from a SQL table have no pipeline, so "Open in Aggregate" stays disabled.

## Watch out
- SQL column names can contain dots. Read a SQL row's value by its top-level key, never through the dotted-path helpers.
- A SQL grid's column kind comes from the tab's cols for Content, and from the selected result set for the Query view (agg=true).

In the code: db::sql::value::stat_value, Worker::sql_column_values (src/db/sql/worker.rs), App::col_data (src/app/column_menu.rs), ColOp::is_stat.
