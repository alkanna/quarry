---
title: "SQL row filters use Sequel Ace's typed operators per column type, combined with the WHERE bar, and SQL tables keep the Mongo multi-select"
author: Khiman
date: 2026-11-09
branch: master
cost: { minutes: 1, tokens: 1363 }
id: l-8d0563c3
scope: repo
status: active
src: agent
touches:
  - src/app/documents.rs
  - src/app/table.rs
  - src/db/dates.rs
---

## Decided
- The user wants SQL filtering to match Sequel Ace: several rules, each with an operator list that depends on the column type (number, text, date, ENUM/SET, BLOB/JSON). The user also wants to keep the WHERE bar with autocomplete, so typed rules and SQL text must turn into each other.
- SQL tables keep the same multi-select as the Mongo table: checkbox column, shift-click ranges, the floating "N selected" bar and a row menu that acts on all checked rows. A bulk "Set value" stages one edit per row, so Save, Discard and Undo still apply.
- Date rules reuse the relative-date shortcuts the Mongo query inputs already understand, next to Sequel Ace's before/after.

## Tried and ruled out
- A free-text WHERE bar alone, or a single Sequel Ace filter row alone: the user wants both typed rules and SQL.

In the code: the multi-select to reuse is toggle_check, selection_bar and docs_menu in src/app/documents.rs, with the CHECK_W column in src/app/table.rs; the relative-date parsing lives in src/db/dates.rs.
