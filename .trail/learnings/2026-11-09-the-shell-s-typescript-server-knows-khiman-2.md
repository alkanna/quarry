---
title: "The Shell's TypeScript server knows each loaded collection's fields and text values"
author: Khiman
date: 2026-11-09
branch: master
cost: { minutes: 14, tokens: 147877 }
id: l-5bd70769
scope: repo
status: active
replaces: l-a2dc8c1d
src: agent
touches:
  - src/db/analysis.rs
  - src/app/schema.rs
  - src/ui/complete.rs
  - src/app/shell_ls.rs
  - src/db/js_ls.rs
  - src/app/poll.rs
  - src/app/config.rs
---

## Decided
Collections open in a tab of the same database are declared to the Shell's language server as Collection<T>, with T built from the tab's field info (current tab first). Text fields are a union of their known values kept open with `string & {}`, and every object type has `[key: string]: any` so fields the page did not load stay allowed. A quote after a field's colon in a query object asks the server, which lists the values. Outside strings, query objects keep the built-in list, which takes the fields of the collection the call is made on (`orders.find`, `db.collection("orders").find`).

## Tried and ruled out
Rebuilding the types on every request: several requests per keystroke, megabytes of text on wide collections. Built-in suggestions from the current tab only: they offered another collection's values.

## Watch out
Tab field info is an Arc that is replaced on every change, never edited in place (no Arc::make_mut): the Shell detects changed fields by pointer identity and holds the previous Arcs so a pointer cannot be reused. Strings, comments and regexes are judged on the caret's line only. Date shortcuts (#today, @...) are Query box syntax and are filtered from the Shell's list.

In the code: ts_type, js_spot, call_target and suggest_js in src/ui/complete.rs; db_fields, js_send and js_ask in src/app/shell_ls.rs; js_globals in src/db/js_ls.rs; merges in src/app/poll.rs and src/app/config.rs. End-to-end: QUARRY_LS_DIR=<copy of the shell folder> cargo test js_ls_query_values -- --ignored
