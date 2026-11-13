---
title: "Whole-query column stats are computed in the app, not by the server, so both scopes agree"
author: Khiman
date: 2026-11-07
branch: master
cost: { minutes: 18, tokens: 157973 }
id: l-831c8913
scope: repo
status: active
src: agent
touches:
  - src/app.rs
  - src/app/column_menu.rs
  - src/app/config.rs
  - src/app/documents.rs
  - src/app/modals.rs
  - src/app/poll.rs
  - src/app/shell.rs
  - src/app/state.rs
  - src/app/table.rs
  - src/app/tree.rs
---

## Decided
- The column popover's whole-query data actions, including count distinct and group by, read the column's values (capped, in the query's sort order) and compute the result the same way as the page scope. Both scopes then treat arrays the same, as whole values.
- The popover sets its close behavior to "close on click outside". Every action closes it explicitly, and a close inside a submenu also closes the parent popover.

## Tried and ruled out
- Sending whole-query count distinct and group by to the server's distinct and $group: they unwind arrays and drop array positions, so switching scope gave different numbers. Two reviews suggested going back to the server; that was rejected for this reason.
- Leaving egui's default menu close behavior: any click inside, such as the This page / Whole query switch, closed the whole popover.

## Watch out
- Whole-query reads stop at 100,000 documents and say so in the result.
- A Group result from the worker comes back as the existing Grouped message, so the group dialog and "Open in Aggregate" work unchanged.

In the code: db::col_op_result and COL_VALUES_CAP; Worker::column_values; the header popover uses egui::PopupCloseBehavior::CloseOnClickOutside.
