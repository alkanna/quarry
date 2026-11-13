---
title: Inline value editors check every keystroke and only let Enter swap in a list row when the typed text is invalid
author: Khiman
date: 2026-11-12
branch: master
cost: { minutes: 5, tokens: 84064 }
id: l-c140d432
scope: repo
status: active
replaces: l-b8947dac
src: agent
touches:
  - src/app/sql/edit.rs
  - src/app/tree.rs
  - src/db/sql/worker.rs
---

## Decided
- The SQL cell editor and staging use the same column check, so the line under the cell never disagrees with what is staged. It covers what the kind alone cannot: DECIMAL scale, integer ranges, text length, ENUM options, days per month, time parts, JSON and UUID.
- Enter on an ENUM or key list takes a row only after the arrows, or the first row when the typed text is invalid and the list answers that exact text. A valid typed value is always saved as typed.
- Words like now and yesterday resolve in the zone the column is read in. TIMESTAMP and timestamptz use the connection's session zone; under the server's own zone Quarry cannot know the clock, so MySQL refuses the words and PostgreSQL gets them as typed.
- The foreign key list learns the label column and table size once; on tables over 100,000 rows it only looks up the exact key, because LIKE on cast text scans every row.
- Mongo dates in Local take the offset of the moment edited, not today's, so dates across daylight saving keep their wall clock.

## Watch out
- egui hands focus over on mouse release, when no press is in the input, so telling a click in a helper from a click outside goes by the pointer position, not the press.
- The page's plain-key navigation is off only while a helper reads keys (calendar, value list, boolean switch).

In the code: src/db/sql/check.rs, src/ui/pickers.rs, sql_cell_editor in src/app/sql/edit.rs, inline_editor in src/app/tree.rs, key_search in src/db/sql/worker.rs.
