---
title: Work triggered by a click in the table grid must run after results() puts the page back in the tab
author: Khiman
date: 2026-11-11
branch: feat/postgres
cost: { minutes: 26, tokens: 173414 }
id: l-e3435b12
scope: repo
status: active
src: agent
touches:
  - src/app/chrome.rs
  - src/app/documents.rs
  - src/app/table.rs
  - src/db/sql/pg_read_tests.rs
  - src/app/sql_edit.rs
  - src/app/tabs.rs
  - src/db/sql/pg.rs
  - src/app/export.rs
  - src/db/copy.rs
  - src/db/sql/dump.rs
---

## Decided
- results() moves the tab's page out of the tab (std::mem::take on t.docs or the view's store) while the grid draws, so self stays editable. Anything the grid triggers that looks rows up in the tab (SQL cell editor open and finish, cell menu actions, group drill, a followed FK link) is returned from doc_table as SqlAfter and run by results() once the page is back.

## Tried and ruled out
- Calling sql_open_cell, sql_cell_finish, sql_cell_acts and sql_drill from inside doc_table: they read an empty t.docs, so the editor was keyed to nothing (invisible on keyed rows, drawn on every keyless Shell row with a widget id clash), a finished edit found no row, and a drill did nothing. It only seemed to work after an unlock prompt, which reran the open later.
- Clearing a leftover editor on Esc: hid the symptom, removed once the ordering was fixed.

## Watch out
- A new handler in doc_table that reads t.docs (or the view store) through self must go in SqlAfter or take the drawn `docs` as a parameter.

In the code: src/app/documents.rs results(), src/app/table.rs SqlAfter and sql_after, src/app/sql_edit.rs sql_open_cell and sql_cell_finish.
