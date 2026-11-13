---
title: PostgreSQL read-only needs a rolled-back transaction per statement because the lexer deny list can never be complete
author: Khiman
date: 2026-11-10
branch: feat/postgres
cost: { minutes: 113, tokens: 432087 }
id: l-334a91a6
scope: repo
status: active
src: agent
touches:
  - docs/sql/POSTGRES.md
  - src/db/sql/export.rs
  - src/db/sql/pg_catalog.rs
  - src/db/sql/pg_write_tests.rs
  - src/db/sql/value.rs
  - src/db/sql/worker.rs
  - src/db/sql/write.rs
  - src/db/sql/lex.rs
  - src/db/sql/lex_pg.rs
  - src/db/sql/relay.rs
---

## Decided
- On a locked PostgreSQL connection every statement with user text runs alone in BEGIN READ ONLY and is always rolled back, never sharing a transaction with another user statement. A pooled connection that ran user text is closed, not returned.
- The lexer refuses side-effect functions both before `(` and after `.`, because `(expr).f` and `alias.f` call `f(expr)`.
- Behind a pooler that refuses startup options (PgBouncer), nothing is set on the session: every transaction starts with SET LOCAL (timeouts, formats, search_path as a quoted identifier), unlocked statements get their own BEGIN READ WRITE, and statements that leave session state (SET, RESET, DISCARD, set_config, LISTEN, PREPARE, WITH HOLD cursors, temp tables) and user transaction control are refused. Stop closes the connection instead of cancelling by pid.

## Tried and ruled out
- Session default_transaction_read_only alone: `SELECT set_config('default_transaction_read_only','off',false)` unlocks the pooled session.
- A deny list alone: ts_stat, ts_rewrite, crosstab and connectby run SQL text the lexer never sees; dblink_exec dropped a table from a "read-only" query.
- Several user statements in one transaction: set_config('client_encoding','SJIS',false) hidden in a string made the server split the next statement differently from the lexer and run an injected BEGIN READ WRITE; DROP.
- DISCARD ALL after user text: it releases advisory locks but not dblink connections opened by a wrapper.
- `SET LOCAL search_path = '<quoted>'`: SET reads a string literal as one schema named with the quotes in it.

## Watch out
- A SECURITY DEFINER or extension function that wraps a side effect is invisible, and non-transactional effects (signals, WAL, slots) are not undone, so a locked connection is only as safe as the role.
- Unlocked runs must not be wrapped for paging or counted: the wrapper rejects data-modifying CTEs and runs side effects 1001 times instead of as typed.
- A result cut at the cap must cancel its backend and drop the connection, or its locks block the next ALTER and everything queued behind it.

In the code: src/db/sql/lex_pg.rs (REFUSED_FUNCTIONS_*), src/db/sql/lex.rs (session_state, transaction_control), src/db/sql/pg_read.rs (read_only, begin_read_only), src/db/sql/run.rs (run_locked, run_on), src/db/sql/relay.rs, src/db/sql/export.rs (stream_locked), src/db/sql/pg.rs (pg_run_locals, pooler); review notes in docs/sql/POSTGRES.md.
