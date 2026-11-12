---
title: "PostgreSQL Server window checks and signals a backend in one statement and knows Quarry's own backends by pid and start"
author: Khiman
date: 2026-11-12
branch: feat/pg-server
cost: { minutes: 476, tokens: 1156156 }
id: l-faf70315
scope: repo
status: active
src: agent
touches:
  - src/db/sql/pg.rs
---

decided: Cancel and Terminate (pg_server::signal) run on the writer in BEGIN READ WRITE, as one statement that reads pg_stat_activity and calls pg_cancel_backend or pg_terminate_backend inside a CASE only when the pid still has the listed backend_start and usename (plus query_start for a cancel, and for any signal behind a pooler) and is not pg_backend_pid(). Quarry's own backends are recorded by pid and backend_start when a pool connection opens (after_connect), when a detached one opens and in pg::prepare (PgPools::own), pruned on each Activity read; the window offers no action on them and signal refuses them. Locks nests a waiter under a blocker that does not wait itself, deeper only when every blocker waits, which is what the design means by queued behind. ruled out: checking in one statement and signalling in a second one of the same transaction: pg_stat_activity is a snapshot taken at the first read of a transaction, so the second statement would trust a stale row. watch out: without pg_read_all_stats another role's row has a NULL backend_start, so it cannot be checked and nothing is sent (the error says pg_read_all_stats, not reused pid); LOCK TABLE outside a transaction block fails at once, so the live lock-chain test uses ALTER TABLE for the exclusive waiter; tests that open sessions tag their query with unique_name and find them by that text, because behind PgBouncer pg_backend_pid in one statement says nothing about the next.
