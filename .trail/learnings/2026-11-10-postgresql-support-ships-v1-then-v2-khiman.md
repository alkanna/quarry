---
title: "PostgreSQL support ships v1 then v2 behind an enum execution boundary, with v3 features hidden"
author: Khiman
date: 2026-11-10
branch: feat/postgres
cost: { minutes: 35, tokens: 107235 }
id: l-76ccadef
scope: repo
status: active
src: agent
touches:
  - src/app.rs
  - src/app/chrome.rs
  - src/app/palette.rs
  - src/ui/theme.rs
  - src/ui/widgets.rs
  - src/app/errors.rs
  - docs/sql/POSTGRES.md
---

## Decided
- Postgres covers v1 (connect, browse, grid edits, read-only Structure, Query, autocomplete, shell) then v2 (structure editing, relations, triggers, import/export, dump, create/drop, Advanced tab). Roles, process list and variables are out of scope and hidden for Postgres connections, not left to fall through to the MySQL or Mongo windows.
- The engine boundary is an enum (Conn, SqlRow, Opts) converting sqlx rows at the edge, consistent with "enums, not traits".
- A Postgres connection gets one lazy pool per database (no USE), capped at 4 live pools of 3 connections.
- On a locked connection every statement with user text runs in BEGIN READ ONLY and is rolled back, on top of the session default and the lexer check.
- The schema level in the sidebar is designed in FigCraft first (3 variants) before any app wiring.
- Servers: PostgreSQL 13 and later, tested on 13 and 18, 13 kept on purpose though past end of life.

## Tried and ruled out
- Making the SQL functions generic over sqlx::Database: higher-ranked bounds everywhere, text vs binary row bytes differ, and the real engine differences still need a match.
- Full parity including v3 in one plan: too large to review.
- Relying on the session read-only default alone: set_config('default_transaction_read_only','off',false) inside a SELECT would unlock the pooled session.

## Watch out
- Postgres refuses text bound to an int column: bound writes need CAST($n AS <format_type>).
- `?` is a jsonb operator, so the shell runner must emit $n on Postgres.
- update_sql and delete_sql append LIMIT 1, which Postgres rejects.

In the code: plan and progress in docs/sql/POSTGRES.md on branch feat/postgres; new files src/db/sql/exec.rs and src/db/sql/pg.rs; Sel gains a schema field; live tests use QUARRY_TEST_POSTGRES and QUARRY_TEST_POSTGRES13.
