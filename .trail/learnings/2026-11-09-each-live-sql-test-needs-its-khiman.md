---
title: Each live SQL test needs its own table because tests run in parallel on one database
author: Khiman
date: 2026-11-09
branch: feat/mysql
cost: { minutes: 13, tokens: 75163 }
id: l-c2debfd7
scope: repo
status: active
src: agent
touches:
  - src/db/sql/worker.rs
  - docs/sql/PLAN.md
  - src/app/documents.rs
  - src/app/export.rs
  - src/app/sql_query.rs
  - src/db/copy.rs
  - src/db/sql/export.rs
  - src/app/demo.rs
  - src/ui/complete.rs
---

## Decided
Live SQL tests all use the quarry_test database on one server and cargo runs them in parallel, so every test creates and drops a table name no other test uses.

## Watch out
Two tests that share a table name with different columns fail at random in a full run (for example "Unknown column 'name'"), while each passes alone. Before adding a live test, grep the table name.

In the code:
- src/db/sql/worker.rs live tests: `live("<table>", "<columns>")` helper and `CREATE TABLE quarry_test.<table>`
- the export test moved from t_big to t_export_big; the field values test keeps t_big
- duplicate check: grep -on 'live("[a-z_0-9]*"' src/db/sql/*.rs | sed 's/.*live("//;s/"//' | sort | uniq -d
