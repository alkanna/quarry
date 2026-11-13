---
title: "Live SQL tests run in parallel on shared servers, so each uses its own tables and never changes global settings"
author: Khiman
date: 2026-11-09
branch: feat/mysql
cost: { minutes: 51, tokens: 134915 }
id: l-8e1ba970
scope: repo
status: active
replaces: l-c2debfd7
src: agent
touches:
  - src/db/sql/import.rs
  - docs/sql/PLAN.md
  - src/app/sql_dump.rs
  - src/app/sql_import.rs
  - src/db/copy.rs
  - src/db/sql/csv.rs
  - src/db/sql/dump.rs
  - src/db/sql/lex.rs
  - src/db/sql/testing.rs
  - src/db/sql/value.rs
---

## Decided
Live SQL tests all use the same MySQL and MariaDB servers and cargo runs them in parallel. Each test creates and drops table and database names no other test uses, and a test that needs a session setting (sql_mode, time_zone) gets it from a pool whose connections run SET SESSION, never from SET GLOBAL.

## Tried and ruled out
SET GLOBAL sql_mode around a test, even restored at the end: every other test that opened a connection in that window got the mode, and since structure editing checks the session sql_mode before running an ALTER, they failed with "the server's sql_mode is now ...". A mutex only serialized the tests that knew about it.

## Watch out
Two tests sharing a table name with different columns fail at random ("Unknown column 'name'") while each passes alone. A full run must be in parallel against both servers to catch these.

In the code:
- src/db/sql/testing.rs `Live::with_session("SET SESSION ...")`; src/db/sql/ddl.rs test `moded()` does the same
- fixed offenders: worker.rs live_no_backslash_escapes_cannot_smuggle_a_statement, import.rs live_values_survive_both_backslash_modes, export test table t_big renamed t_export_big
- check: grep -rn 'SET GLOBAL' src (only lex.rs test inputs should match)
