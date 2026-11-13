---
title: "run_shell asks to unlock while run_sql runs read-only, and agent changes without undo finish after a cancel"
author: Khiman
date: 2026-11-13
branch: fix/agents-review
cost: { minutes: 65, tokens: 715334 }
id: l-c049cc93
scope: repo
status: active
replaces: l-c10ddfeb
src: agent
touches:
  - src/agents/policy.rs
  - src/agents/tools.rs
  - src/app/agents/exec.rs
  - src/db/agent.rs
  - src/db/sql/agent.rs
  - src/app/sidebar.rs
---

## Decided
- run_sql runs read-only on a locked connection and never asks to unlock: the database holds it there (BEGIN READ ONLY on PostgreSQL, a read-only session on MySQL). run_shell asks to unlock like a write, and Allow unlocks: its read-only guard patches the driver inside the Bun process the script controls, so the approval must never say it runs read-only. policy::decide exempts only Call::RunSql from the unlock check.
- An approved run_sql runs with the lock it had when it was queued (Pending.writable, set in agent_queue), and agent_run_writable never escalates: unlocking while the dialog is open keeps it read-only, locking meanwhile makes it read-only.
- run_command is classed by its command (tools::command_class): create, drop, createIndexes, dropIndexes, collMod, renameCollection, dropDatabase, convertToCapped and an aggregate with $out or $merge anywhere are Change structure, so they always ask and never take a session grant. An explain is classed by what it wraps.
- create_table column types follow a strict grammar (tools::col_type: a type name, only unsigned, zerofill, varying, precision, with, without, time, zone after it, one (n) or (n, m)), checked again in structure_sql, and the built statement must lex as one CREATE TABLE whose column list closes at its end under every reading. A character allow-list was not enough: MySQL runs CREATE TABLE t (c int) SELECT ... as one statement.
- Structure tools and scripts still run their own commands, never Worker::db_change, sql_alter or sql_drop, which answer through the user's undo list and toasts. Changes no undo covers (structure tools, Change structure commands, run_sql on an unlocked connection, run_shell) count as writes in Running: a cancel lets them finish, Done records how they ended, and the App refreshes the sidebar through App::refresh_db, the same path as Refresh in the sidebar.
- run_shell output loses the connection's URI, user:password and password, as written and percent-decoded, plus the proxy password (db::Redact, built on the UI thread beside the job, never logged), then the value detectors run. A bare password under 3 characters is only taken out inside the pair and the URI. Log and result together stay under Limits.bytes; the log is cut as lines arrive and gives way to the result.
- A MongoDB structure change that outlives the outer timeout answers Still running on the server, not Failed, and the sidebar looks again 30 s later. createIndexes and dropIndexes take maxTimeMS; create gets it too and is sent again without it when the server refuses the field.

## Watch out
- Package preparation for the agent's shell and the Shell tab is already serialised: both go through run_shell, whose prepare_shell_dir holds a process-wide tokio mutex.
- The main connection's collections are globals in the shell, and db is the driver's Db: db.users is undefined there, so examples use db.collection("users").
- Keep the oneshot sender of shell_runner alive for the whole run: a dropped sender reads as Stop. A cancel or the time limit drops the future, and kill_on_drop kills Bun.
