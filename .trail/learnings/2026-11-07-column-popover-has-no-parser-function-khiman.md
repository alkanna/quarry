---
title: "Column popover has no parser function, and its data actions offer both page and whole-query scope"
author: Khiman
date: 2026-11-07
branch: master
cost: { minutes: 8, tokens: 200007 }
id: l-d1522abe
scope: repo
status: active
src: agent
touches:
  - src/app/config.rs
  - src/app/state.rs
  - src/app/column_menu.rs
  - src/app/modals.rs
  - src/app/table.rs
---

## Decided
- The column header popover (like Mingo's) leaves out the "parser function" that reformats cell values. The user dropped it.
- Column data actions (copy, distinct, sum, average, median, group by) offer a "This page / Whole query" switch, because the user wanted both. Whole query reads at most 100,000 documents and says so when it hits the cap.
- Rename, cast and update values do not write directly. They open the Shell with a ready updateMany script, so the existing write confirmation still applies.

## Tried and ruled out
- Running parser functions under Bun: the only JS runtime the app has is the Bun shell runner. A batch call per page adds latency and requires Bun to be installed.
- Embedding QuickJS (rquickjs): adds a C dependency to every build target.
- Rendering HTML returned by a parser: egui cannot render HTML. Only stripping tags or a small rich-text subset was possible.

## Watch out
- Bringing the parser back means picking a JS engine first. Neither option above was accepted.

In the code: src/app/column_menu.rs (popover, ColOp, col_op_result), Msg::ColumnValues and Worker::column_values (cap COL_VALUES_CAP = 100_000), ColPrefs.align and ColPrefs.labels.
