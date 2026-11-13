---
title: "The Shell's TypeScript server knows each loaded collection's fields and text values"
author: Khiman
date: 2026-11-09
branch: master
cost: { minutes: 1, tokens: 1376 }
id: l-a2dc8c1d
scope: repo
status: active
src: agent
touches:
  - src/ui/complete.rs
  - src/app/shell_ls.rs
  - src/db/js_ls.rs
---

## Decided
Collections open in a tab of the same database are declared to the Shell's language server as Collection<T>, with T built from that tab's field info. Text fields are a union of their known values kept open with `string & {}`, so typing a quote after a field's colon in find({...}) lists the values, and found documents get typed fields. Inside a string the Shell asks the server; outside strings, query objects keep the built-in list, which gets the tab's field info so value shortcuts suit the field's kind like in the Query box.

## Tried and ruled out
Built-in suggestions only: they cannot follow which collection a script queries, while the declared types do.

## Watch out
Types come from what a tab has loaded, so a collection reached through db.collection("x") is never typed. The built-in list in the Shell uses the current tab's fields whatever collection the script names. Date shortcuts (#today, @...) can reach the Shell's list right after a date field's colon, so they are filtered there; the filter is not dead code.

In the code: ts_type and suggest_js in src/ui/complete.rs, js_send and js_ask in src/app/shell_ls.rs, js_globals in src/db/js_ls.rs. End-to-end check: QUARRY_LS_DIR=<copy of the shell folder> cargo test js_ls_query_values -- --ignored
