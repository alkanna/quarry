---
title: "Agent structure tools and scripts bypass the user's write messages, and free SQL and the shell run read-only when locked"
author: Khiman
date: 2026-11-13
branch: worktree-agent-a8010dba212b9d70b
cost: { minutes: 40, tokens: 566480 }
id: l-c10ddfeb
scope: repo
status: active
src: agent
touches:
  - src/agents/policy.rs
  - src/app/agents/exec.rs
  - src/db/agent.rs
  - src/db/sql/agent.rs
  - src/agents/tools.rs
---

## Decided
- run_sql and run_shell never ask to unlock. On a locked connection they run read-only like the user's own SQL and shell, and the approval says so; policy::decide leaves them out of the unlock check. run_command still needs the unlock, since its allow-list holds create, drop, collMod and aggregate with $out.
- Structure tools run their own commands (create, drop, createIndexes, dropIndexes on MongoDB; one CREATE or DROP statement Quarry writes, run through sql::run::run, on SQL) instead of Worker::db_change, drop_index, sql_alter or sql_drop. Those answer through Msg::DbChanged, Written or SqlDdl, which toast and push onto the user's undo list. After one finishes, the App lists the collections or tables again.
- create_index and drop_index take `table` on SQL; index keys are 1 or -1 there.
- run_shell's job is built on the UI thread, which alone reads the address from the keychain, and never in an ephemeral app. Its output goes through the value rules only, which the approval calls best effort.

## Watch out
- Keep the oneshot sender of shell_runner alive for the whole run: a dropped sender reads as Stop. A cancel or the time limit drops the future, and kill_on_drop kills Bun.
