---
title: Inline value editors check every keystroke and only let Enter swap in a list row when the typed text is invalid
author: Khiman
date: 2026-11-12
branch: master
cost: { minutes: 1, tokens: 0 }
id: l-b8947dac
scope: repo
status: active
src: agent
touches:
  - src/app/sql/edit.rs
  - src/app/tree.rs
  - src/db/sql/worker.rs
---

## Decided
- The SQL cell editor checks the text against its column as it is typed, and staging goes through that same check, because a second parser could disagree with what the line under the cell promised. The check covers what the kind alone cannot: DECIMAL scale and digits (with a rounded fix), integer ranges per type, VARCHAR and CHAR length, ENUM options, days per month, time part limits, JSON position and UUID digit count. Words like yesterday 18:00, today, now and HH:MM are resolved on this machine so the line can say what they save as.
- Helpers (calendar, ENUM list, SET checklist, foreign key list, true/false/NULL switch) are egui Areas under the cell, not windows. The editor keeps the rect they covered the frame before, so a click inside them is not taken as a click outside, which commits.
- Enter on an ENUM or key list takes the highlighted row only after the arrows were used, or the first row when the typed text is not a valid value and the list answers this exact text. A valid typed value is saved as typed, so a quick Enter never swaps in another row.
- The foreign key list sends one lookup at a time and asks again with the newest text when the answer arrives, instead of a timer.
- Mongo dates are edited in UTC or local time with a switch; the text keeps its offset, so the same moment is staged either way.

## Watch out
- The boolean switch and the calendar leave no text field focused, so the page's plain-key navigation (arrows, Page Up and Down) is off while a cell or inline editor is open.
- TINYINT(1) on MySQL is treated as a boolean and saves 1 and 0.

In the code: src/db/sql/check.rs, src/ui/pickers.rs, sql_cell_editor and sql_key_lookup in src/app/sql/edit.rs, inline_editor in src/app/tree.rs, key_search in src/db/sql/worker.rs.
