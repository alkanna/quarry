---
title: Actions on every matching document ignore the limit and skip and are blocked while a collation is set
author: Khiman
date: 2026-11-08
branch: feat/find-bar
cost: { minutes: 33, tokens: 365238 }
id: l-4924fd22
scope: repo
status: active
src: agent
touches:
  - ".release-please-manifest.json"
  - CHANGELOG.md
  - Cargo.lock
  - Cargo.toml
  - src/app/state.rs
  - src/app/documents.rs
  - src/app/modals.rs
  - src/app/query.rs
---

## Decided
Update matching, Delete all matching and Rename in all matching documents act on the filter alone. They never apply the limit, skip or collation from the find bar's option chips.
- Their confirmation does not show the results count when a limit or skip is set, because that count stops at the limit. It says "every match, ignoring the limit and skip" instead.
- While the last-run query has a collation, these actions are disabled with a tooltip. Without the collation they would match a different set of documents than the table shows.

## Watch out
Any new bulk action should take its count from `Tab::bulk_count()`, never from `t.count`. A review caught "about 50 documents" in a delete confirmation that would really have deleted millions.

In the code: Tab::bulk_count and Tab::clear_options in src/app/state.rs; the bulk menu items in src/app/documents.rs; the rename in src/app/modals.rs.
