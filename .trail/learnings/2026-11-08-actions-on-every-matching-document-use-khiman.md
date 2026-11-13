---
title: Actions on every matching document use the filter and collation but ignore the limit and skip
author: Khiman
date: 2026-11-08
branch: feat/find-bar
cost: { minutes: 47, tokens: 251896 }
id: l-ee6fb638
scope: repo
status: active
replaces: l-4924fd22
src: agent
touches:
  - src/db.rs
  - src/app/state.rs
---

## Decided
These actions all work on the shown query's filter and collation, passed together as one db::Scope (Query::scope()):
- Update matching, Delete all matching, Rename in all matching
- Group by, Distinct, Schema
- The column menu's data actions and its update scripts

Because they get the collation too, they act on the same documents the table shows. They deliberately ignore the find bar's limit and skip. Their confirmation does not show the results count when a limit or skip is set, since that count stops at the limit. It says "every match, ignoring the limit and skip" instead.

## Tried and ruled out
Disabling these actions while a collation was set. The review called it a patch on each call site, and it hid working features. Every driver call involved accepts a collation.

## Watch out
- Any new action on "documents matching the filter" should take Tab.query.scope(), never the filter alone. Without the collation, it changes a different set of documents than the user sees.
- Its count must come from Tab::bulk_count(), never t.count. A review caught "about 50 documents" in a delete confirmation that would really have deleted millions.

In the code: db::Scope and Query::scope in src/db.rs; Tab::bulk_count in src/app/state.rs; Danger::DeleteMatching, RenameMany and UpdateMany; the worker functions delete_many, update_many, rename_many, update_preview, distinct_count, schema and column_values.
