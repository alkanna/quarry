---
title: "PostgreSQL trigger editing: what the catalog and the UI tests did not say"
author: Khiman
date: 2026-11-11
branch: worktree-agent-a422d47b3cf144b7a
cost: { minutes: 90, tokens: 216887 }
id: l-1c3ace0f
scope: repo
status: active
src: agent
touches:
  - "src/db/sql/pg_catalog.rs#TRIGGERS"
  - "src/app/sql_alter.rs#sql_altered"
---

decided: a trigger and its function are one transaction; the editor reads both with the same SQL the transaction re-reads under the lock (seen_trig, seen_func in pg_catalog), so any other session's change stops the save and guards the undo. A function is rewritten by replacing only the body inside the pg_get_functiondef text, so SECURITY DEFINER and SET clauses stay. ruled out: parsing pg_get_triggerdef to rebuild a trigger (refuse constraint, inherited, partitioned, argument and transition table triggers instead). watch out: a PostgreSQL catalog bool must be cast ::int, because the row reader's flag() reads an int and a raw bool column reads as false (it silently made every function look foreign-owned); a PostgreSQL query may not contain a question mark, so no (?:...) in a regex; before PostgreSQL 14 the clone of a trigger on a partition has tgisinternal set and is not listed; a structure read already in flight when a change answers used to land over the reload, so sql_altered clears Tab::idx_req; in a UI test the Recent changes dialog stays open after an undo and swallows clicks, so click Close before the next step, and open the table after making a trigger because the structure is read once when it opens.
