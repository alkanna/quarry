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
  - src/db/sql/pg_server.rs
  - src/app/pg_server.rs
---

decided: Cancel and Terminate (pg_server::signal) run on a held connection in BEGIN READ ONLY (a hot standby refuses READ WRITE with 25006, and neither function writes; the unlock is Quarry's own gate), as one statement that reads pg_stat_activity and calls pg_cancel_backend or pg_terminate_backend inside a CASE only when the pid still has the listed backend_start and usename (plus query_start for a cancel, and for any signal behind a pooler) and is not pg_backend_pid(). Quarry's own backends are recorded when a pool connection opens (after_connect), when a detached one opens and in pg::prepare, in a process-wide registry (pg::note_own) keyed by server (postmaster start), pid and backend_start, both as to_char UTC text because ::text follows each connection's TimeZone; host and port were ruled out as the server key since SSH tunnels and the Cloud SQL proxy give each connection 127.0.0.1 and its own port. An Activity read prunes only entries noted before it began (a sequence number), and a row whose start is hidden keeps its entry. Behind a pooler nothing is recorded and a session with the connection's application_name counts as Quarry's. The window offers no action on them and signal refuses them. Confirm buttons ignore clicks for 0.5 s of egui time after arming (sql_server::confirmed), since they are drawn where the arming button was. Locks nests a waiter under a blocker that does not wait itself, deeper only when every blocker waits, which is what the design means by queued behind. ruled out: checking in one statement and signalling in a second one of the same transaction: pg_stat_activity is a snapshot taken at the first read of a transaction, so the second statement would trust a stale row. watch out: without pg_read_all_stats another role's row has a NULL backend_start, so it cannot be checked and nothing is sent (the error says pg_read_all_stats, not reused pid); LOCK TABLE outside a transaction block fails at once, so the live lock-chain test uses ALTER TABLE for the exclusive waiter; tests that open sessions tag their query with unique_name and find them by that text, because behind PgBouncer pg_backend_pid in one statement says nothing about the next.
